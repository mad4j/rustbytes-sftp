use clap::Parser;
use log::{error, info, warn, LevelFilter};
use russh::keys::ssh_key::rand_core::OsRng;
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::fs;
use tokio::io::AsyncReadExt;

/// Configurazione da linea di comando
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Indirizzo IP su cui ascoltare
    #[arg(short, long, default_value = "0.0.0.0")]
    host: String,

    /// Porta su cui ascoltare
    #[arg(short, long, default_value = "22")]
    port: u16,

    /// Username per autenticazione password
    #[arg(long, default_value = "admin")]
    username: String,

    /// Password per autenticazione password
    #[arg(long, default_value = "password")]
    password: String,

    /// Directory radice per le operazioni SFTP
    #[arg(long, default_value = ".")]
    root_dir: PathBuf,
}

#[derive(Clone)]
struct Server {
    config: Arc<ServerConfig>,
}

#[derive(Debug)]
struct ServerConfig {
    username: String,
    password: String,
    root_dir: PathBuf,
}

impl russh::server::Server for Server {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshSession::new(self.config.clone())
    }
}

struct SshSession {
    clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    config: Arc<ServerConfig>,
}

impl SshSession {
    fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    pub async fn get_channel(&mut self, channel_id: ChannelId) -> Channel<Msg> {
        let mut clients = self.clients.lock().await;
        clients.remove(&channel_id).unwrap()
    }
}

impl russh::server::Handler for SshSession {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        info!("credentials: {}, {}", user, password);
        if user == self.config.username && password == self.config.password {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::Reject { proceed_with_methods: None, partial_success: false })
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        info!("credentials: {}, {:?}", user, public_key);
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        {
            let mut clients = self.clients.lock().await;
            clients.insert(channel.id(), channel);
        }
        Ok(true)
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        info!("subsystem: {}", name);

        if name == "sftp" {
            let channel = self.get_channel(channel_id).await;
            let sftp = SftpSession::new(self.config.clone());
            session.channel_success(channel_id)?;
            russh_sftp::server::run(channel.into_stream(), sftp).await;
        } else {
            session.channel_failure(channel_id)?;
        }

        Ok(())
    }
}

struct SftpSession {
    version: Option<u32>,
    root_dir: PathBuf,
    open_files: HashMap<String, tokio::fs::File>,
    open_dirs: HashMap<String, tokio::fs::ReadDir>,
    handle_counter: u32,
}

impl SftpSession {
    fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            version: None,
            root_dir: config.root_dir.clone(),
            open_files: HashMap::new(),
            open_dirs: HashMap::new(),
            handle_counter: 0,
        }
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let path = if path.starts_with('/') {
            Path::new(path).strip_prefix("/").unwrap_or(Path::new(path))
        } else {
            Path::new(path)
        };
        
        let resolved = self.root_dir.join(path);
        
        // Controllo di sicurezza: il percorso deve rimanere dentro root_dir
        match resolved.canonicalize() {
            Ok(canonical) => {
                if canonical.starts_with(&self.root_dir) {
                    Ok(canonical)
                } else {
                    warn!("Tentativo di accesso fuori dalla root directory: {:?}", canonical);
                    Err(StatusCode::PermissionDenied)
                }
            }
            Err(_) => {
                // Se il path non esiste ancora, controlliamo il parent
                if let Some(parent) = resolved.parent() {
                    match parent.canonicalize() {
                        Ok(canonical_parent) => {
                            if canonical_parent.starts_with(&self.root_dir) {
                                Ok(resolved)
                            } else {
                                warn!("Tentativo di accesso fuori dalla root directory: {:?}", resolved);
                                Err(StatusCode::PermissionDenied)
                            }
                        }
                        Err(_) => Err(StatusCode::NoSuchFile),
                    }
                } else {
                    Err(StatusCode::NoSuchFile)
                }
            }
        }
    }

    fn next_handle(&mut self) -> String {
        self.handle_counter += 1;
        format!("handle_{}", self.handle_counter)
    }

    async fn metadata_to_file_attributes(metadata: &std::fs::Metadata) -> FileAttributes {
        let mut attrs = FileAttributes::default();
        attrs.size = Some(metadata.len());
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            attrs.uid = Some(metadata.uid());
            attrs.gid = Some(metadata.gid());
            attrs.permissions = Some(metadata.mode());
        }
        
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                attrs.mtime = Some(duration.as_secs() as u32);
            }
        }
        
        if let Ok(accessed) = metadata.accessed() {
            if let Ok(duration) = accessed.duration_since(std::time::UNIX_EPOCH) {
                attrs.atime = Some(duration.as_secs() as u32);
            }
        }
        
        attrs
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        if self.version.is_some() {
            error!("duplicate SSH_FXP_VERSION packet");
            return Err(StatusCode::ConnectionLost);
        }

        self.version = Some(version);
        info!("version: {:?}, extensions: {:?}", self.version, extensions);
        Ok(Version::new())
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        info!("close handle: {}", handle);
        
        // Rimuovi il file o directory dal tracking
        if self.open_files.remove(&handle).is_some() {
            info!("Closed file handle: {}", handle);
        } else if self.open_dirs.remove(&handle).is_some() {
            info!("Closed directory handle: {}", handle);
        }

        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        info!("opendir: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match fs::read_dir(&resolved_path).await {
            Ok(read_dir) => {
                let handle = self.next_handle();
                self.open_dirs.insert(handle.clone(), read_dir);
                Ok(Handle { id, handle })
            }
            Err(e) => {
                warn!("Failed to open directory {:?}: {}", resolved_path, e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                    _ => Err(StatusCode::Failure),
                }
            }
        }
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        info!("readdir handle: {}", handle);
        
        if let Some(read_dir) = self.open_dirs.get_mut(&handle) {
            let mut files = Vec::new();
            
            // Leggi alcuni file dalla directory
            for _ in 0..10 {  // Leggi massimo 10 file per volta
                match read_dir.next_entry().await {
                    Ok(Some(entry)) => {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        match entry.metadata().await {
                            Ok(metadata) => {
                                let attrs = Self::metadata_to_file_attributes(&metadata).await;
                                files.push(File::new(file_name, attrs));
                            }
                            Err(_) => {
                                files.push(File::new(file_name, FileAttributes::default()));
                            }
                        }
                    }
                    Ok(None) => break,  // Fine directory
                    Err(_) => break,
                }
            }
            
            if files.is_empty() {
                Err(StatusCode::Eof)
            } else {
                Ok(Name { id, files })
            }
        } else {
            Err(StatusCode::BadMessage)
        }
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        info!("realpath: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match resolved_path.canonicalize() {
            Ok(canonical) => {
                // Converti il path canonico in un path relativo alla root
                let relative_path = if let Ok(rel) = canonical.strip_prefix(&self.root_dir) {
                    format!("/{}", rel.to_string_lossy())
                } else {
                    "/".to_string()
                };
                
                Ok(Name {
                    id,
                    files: vec![File::dummy(&relative_path)],
                })
            }
            Err(_) => {
                Ok(Name {
                    id,
                    files: vec![File::dummy("/")],
                })
            }
        }
    }

    async fn open(&mut self, id: u32, path: String, _pflags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, Self::Error> {
        info!("open file: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match fs::File::open(&resolved_path).await {
            Ok(file) => {
                let handle = self.next_handle();
                self.open_files.insert(handle.clone(), file);
                Ok(Handle { id, handle })
            }
            Err(e) => {
                warn!("Failed to open file {:?}: {}", resolved_path, e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                    _ => Err(StatusCode::Failure),
                }
            }
        }
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<russh_sftp::protocol::Data, Self::Error> {
        info!("read handle: {}, offset: {}, len: {}", handle, offset, len);
        
        if let Some(file) = self.open_files.get_mut(&handle) {
            use tokio::io::AsyncSeekExt;
            
            match file.seek(std::io::SeekFrom::Start(offset)).await {
                Ok(_) => {
                    let mut buffer = vec![0u8; len as usize];
                    match file.read(&mut buffer).await {
                        Ok(bytes_read) => {
                            buffer.truncate(bytes_read);
                            if bytes_read == 0 {
                                Err(StatusCode::Eof)
                            } else {
                                Ok(russh_sftp::protocol::Data { id, data: buffer })
                            }
                        }
                        Err(_) => Err(StatusCode::Failure),
                    }
                }
                Err(_) => Err(StatusCode::Failure),
            }
        } else {
            Err(StatusCode::BadMessage)
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("stat: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match fs::metadata(&resolved_path).await {
            Ok(metadata) => {
                let attrs = Self::metadata_to_file_attributes(&metadata).await;
                Ok(russh_sftp::protocol::Attrs { id, attrs })
            }
            Err(e) => {
                warn!("Failed to stat {:?}: {}", resolved_path, e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                    _ => Err(StatusCode::Failure),
                }
            }
        }
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("lstat: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match fs::symlink_metadata(&resolved_path).await {
            Ok(metadata) => {
                let attrs = Self::metadata_to_file_attributes(&metadata).await;
                Ok(russh_sftp::protocol::Attrs { id, attrs })
            }
            Err(e) => {
                warn!("Failed to lstat {:?}: {}", resolved_path, e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                    _ => Err(StatusCode::Failure),
                }
            }
        }
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("fstat handle: {}", handle);
        
        if let Some(file) = self.open_files.get(&handle) {
            match file.metadata().await {
                Ok(metadata) => {
                    let attrs = Self::metadata_to_file_attributes(&metadata).await;
                    Ok(russh_sftp::protocol::Attrs { id, attrs })
                }
                Err(_) => Err(StatusCode::Failure),
            }
        } else {
            Err(StatusCode::BadMessage)
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .init();

    // Parsing degli argomenti da linea di comando
    let args = Args::parse();

    // Verifica che la directory radice esista
    if !args.root_dir.exists() {
        error!("Root directory {:?} does not exist", args.root_dir);
        std::process::exit(1);
    }

    if !args.root_dir.is_dir() {
        error!("Root directory {:?} is not a directory", args.root_dir);
        std::process::exit(1);
    }

    // Canonicalizza il percorso della root directory
    let root_dir = match args.root_dir.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            error!("Failed to canonicalize root directory {:?}: {}", args.root_dir, e);
            std::process::exit(1);
        }
    };

    info!("SFTP root directory: {:?}", root_dir);

    let server_config = Arc::new(ServerConfig {
        username: args.username,
        password: args.password,
        root_dir,
    });

    let config = russh::server::Config {
        auth_rejection_time: Duration::from_secs(3),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![
            russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519).unwrap(),
        ],
        ..Default::default()
    };

    let mut server = Server {
        config: server_config,
    };

    info!("Starting SFTP server on {}:{}", args.host, args.port);

    server
        .run_on_address(
            Arc::new(config),
            (args.host.as_str(), args.port),
        )
        .await
        .unwrap();
}