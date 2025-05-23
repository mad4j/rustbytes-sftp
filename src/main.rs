use args::Args;
use clap::Parser;
use log::{error, info, warn, LevelFilter};
use russh::keys::ssh_key::rand_core::OsRng;
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use server::{Server, ServerConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

mod args;
mod server;


pub struct SshSession {
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

#[derive(Debug)]
struct OpenFile {
    file: tokio::fs::File,
    path: PathBuf,
    is_binary: bool,
}

impl OpenFile {
    async fn new(path: PathBuf) -> Result<Self, std::io::Error> {
        let file = fs::File::open(&path).await?;
        let is_binary = Self::detect_binary_file(&path).await;
        
        info!("Opened file: {:?}, binary: {}", path, is_binary);
        
        Ok(Self {
            file,
            path,
            is_binary,
        })
    }

    async fn detect_binary_file(path: &Path) -> bool {
        // Controllo basato sull'estensione del file
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            let binary_extensions = [
                "exe", "dll", "so", "dylib", "bin", "dat", "db", "sqlite", "sqlite3",
                "jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp", "ico",
                "mp3", "wav", "ogg", "flac", "aac", "m4a",
                "mp4", "avi", "mkv", "mov", "wmv", "flv", "webm",
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
                "zip", "rar", "7z", "tar", "gz", "bz2", "xz",
                "obj", "lib", "a", "deb", "rpm", "dmg", "iso"
            ];
            
            if binary_extensions.contains(&extension.to_lowercase().as_str()) {
                return true;
            }
        }

        // Controllo del contenuto del file (primi 512 bytes)
        if let Ok(mut file) = fs::File::open(path).await {
            let mut buffer = [0u8; 512];
            if let Ok(bytes_read) = file.read(&mut buffer).await {
                let sample = &buffer[..bytes_read];
                
                // Se contiene byte nulli, probabilmente è binario
                if sample.contains(&0) {
                    return true;
                }
                
                // Controlla la percentuale di caratteri non-ASCII
                let non_ascii_count = sample.iter().filter(|&&b| b > 127).count();
                let non_ascii_percentage = (non_ascii_count as f32 / bytes_read as f32) * 100.0;
                
                // Se più del 30% dei caratteri sono non-ASCII, probabilmente è binario
                if non_ascii_percentage > 30.0 {
                    return true;
                }
            }
        }

        false
    }
}

struct SftpSession {
    version: Option<u32>,
    root_dir: PathBuf,
    open_files: HashMap<String, OpenFile>,
    open_dirs: HashMap<String, tokio::fs::ReadDir>,
    handle_counter: u32,
    max_read_size: u32,
}

impl SftpSession {
    fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            version: None,
            root_dir: config.root_dir.clone(),
            open_files: HashMap::new(),
            open_dirs: HashMap::new(),
            handle_counter: 0,
            max_read_size: config.max_read_size,
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
            
            // Imposta correttamente i permessi e il tipo di file
            let mut mode = metadata.mode();
            
            // Assicurati che i bit del tipo di file siano impostati correttamente
            if metadata.is_file() {
                mode |= 0o100000; // S_IFREG - file regolare
            } else if metadata.is_dir() {
                mode |= 0o040000; // S_IFDIR - directory
            } else if metadata.file_type().is_symlink() {
                mode |= 0o120000; // S_IFLNK - link simbolico
            }
            
            attrs.permissions = Some(mode);
        }
        
        #[cfg(windows)]
        {
            // Su Windows, imposta permessi di base
            let mut mode = 0o644; // rw-r--r-- per file
            if metadata.is_dir() {
                mode = 0o755 | 0o040000; // rwxr-xr-x + directory flag
            } else if metadata.is_file() {
                mode = 0o644 | 0o100000; // rw-r--r-- + regular file flag
            }
            attrs.permissions = Some(mode);
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

    async fn format_longname(filename: &str, metadata: &std::fs::Metadata) -> String {
        let _file_type = if metadata.is_dir() {
            'd'
        } else if metadata.file_type().is_symlink() {
            'l'
        } else {
            '-'
        };
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mode = metadata.mode();
            let permissions = format!(
                "{}{}{}{}{}{}{}{}{}",
                file_type,
                if mode & 0o400 != 0 { 'r' } else { '-' },
                if mode & 0o200 != 0 { 'w' } else { '-' },
                if mode & 0o100 != 0 { 'x' } else { '-' },
                if mode & 0o040 != 0 { 'r' } else { '-' },
                if mode & 0o020 != 0 { 'w' } else { '-' },
                if mode & 0o010 != 0 { 'x' } else { '-' },
                if mode & 0o004 != 0 { 'r' } else { '-' },
                if mode & 0o002 != 0 { 'w' } else { '-' },
                if mode & 0o001 != 0 { 'x' } else { '-' },
            );
            
            let nlink = metadata.nlink();
            let uid = metadata.uid();
            let gid = metadata.gid();
            let size = metadata.len();
            
            // Formato data semplificato
            let mtime = if let Ok(modified) = metadata.modified() {
                let datetime = chrono::DateTime::<chrono::Utc>::from(modified);
                datetime.format("%b %d %H:%M").to_string()
            } else {
                "Jan  1 00:00".to_string()
            };
            
            format!("{} {:3} {:5} {:5} {:8} {} {}", 
                    permissions, nlink, uid, gid, size, mtime, filename)
        }
        
        #[cfg(windows)]
        {
            let permissions = if metadata.is_dir() {
                "drwxr-xr-x"
            } else {
                "-rw-r--r--"
            };
            
            let size = metadata.len();
            format!("{} 1 root root {:8} Jan  1 00:00 {}", 
                    permissions, size, filename)
        }
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
        if let Some(open_file) = self.open_files.remove(&handle) {
            info!("Closed file handle: {} (path: {:?}, binary: {})", 
                  handle, open_file.path, open_file.is_binary);
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
                                let longname = Self::format_longname(&file_name, &metadata).await;
                                files.push(File {
                                    filename: file_name,
                                    longname,
                                    attrs,
                                });
                            }
                            Err(e) => {
                                warn!("Failed to get metadata for {}: {}", file_name, e);
                                // Crea attributi di default per file regolare
                                let mut attrs = FileAttributes::default();
                                attrs.permissions = Some(0o100644); // File regolare con permessi rw-r--r--
                                files.push(File {
                                    filename: file_name.clone(),
                                    longname: format!("-rw-r--r-- 1 root root 0 Jan  1 00:00 {}", file_name),
                                    attrs,
                                });
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

    async fn open(&mut self, id: u32, path: String, pflags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, Self::Error> {
        info!("open file: {} with flags: {:?}", path, pflags);
        
        let resolved_path = self.resolve_path(&path)?;
        
        // Controlla se il file esiste
        if !resolved_path.exists() {
            warn!("File does not exist: {:?}", resolved_path);
            return Err(StatusCode::NoSuchFile);
        }

        // Controlla se è un file regolare
        if !resolved_path.is_file() {
            warn!("Path is not a regular file: {:?}", resolved_path);
            return Err(StatusCode::Failure);
        }

        match OpenFile::new(resolved_path).await {
            Ok(open_file) => {
                let handle = self.next_handle();
                info!("Successfully opened file with handle: {} (binary: {})", 
                      handle, open_file.is_binary);
                self.open_files.insert(handle.clone(), open_file);
                Ok(Handle { id, handle })
            }
            Err(e) => {
                warn!("Failed to open file: {}", e);
                match e.kind() {
                    std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                    std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                    _ => Err(StatusCode::Failure),
                }
            }
        }
    }

    /// Implementazione migliorata del comando READ per supportare download di file di testo e binari
    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<russh_sftp::protocol::Data, Self::Error> {
        info!("read handle: {}, offset: {}, requested len: {}", handle, offset, len);
        
        if let Some(open_file) = self.open_files.get_mut(&handle) {
            // Limita la dimensione della lettura al massimo configurato
            let actual_len = std::cmp::min(len, self.max_read_size);
            
            match open_file.file.seek(std::io::SeekFrom::Start(offset)).await {
                Ok(actual_offset) => {
                    if actual_offset != offset {
                        warn!("Seek to {} resulted in position {}", offset, actual_offset);
                    }
                    
                    let mut buffer = vec![0u8; actual_len as usize];
                    match open_file.file.read(&mut buffer).await {
                        Ok(bytes_read) => {
                            if bytes_read == 0 {
                                info!("End of file reached for handle: {}", handle);
                                Err(StatusCode::Eof)
                            } else {
                                buffer.truncate(bytes_read);
                                
                                info!("Successfully read {} bytes from handle: {} (binary: {}, offset: {})", 
                                      bytes_read, handle, open_file.is_binary, offset);
                                
                                // Log aggiuntivo per file di testo (primi 100 caratteri se non binario)
                                if !open_file.is_binary && bytes_read > 0 {
                                    let preview = String::from_utf8_lossy(&buffer[..std::cmp::min(100, bytes_read)]);
                                    info!("Text file preview: {}", preview.chars().take(50).collect::<String>());
                                }
                                
                                Ok(russh_sftp::protocol::Data { id, data: buffer })
                            }
                        }
                        Err(e) => {
                            error!("Failed to read from file handle {}: {}", handle, e);
                            Err(StatusCode::Failure)
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to seek in file handle {}: {}", handle, e);
                    Err(StatusCode::Failure)
                }
            }
        } else {
            warn!("Invalid file handle: {}", handle);
            Err(StatusCode::BadMessage)
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("stat: {}", path);
        
        let resolved_path = self.resolve_path(&path)?;
        
        match fs::metadata(&resolved_path).await {
            Ok(metadata) => {
                let attrs = Self::metadata_to_file_attributes(&metadata).await;
                info!("stat result for {:?}: size={:?}, is_file={}", 
                      resolved_path, attrs.size, metadata.is_file());
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
        
        if let Some(open_file) = self.open_files.get(&handle) {
            match open_file.file.metadata().await {
                Ok(metadata) => {
                    let attrs = Self::metadata_to_file_attributes(&metadata).await;
                    Ok(russh_sftp::protocol::Attrs { id, attrs })
                }
                Err(e) => {
                    error!("Failed to get metadata for handle {}: {}", handle, e);
                    Err(StatusCode::Failure)
                }
            }
        } else {
            warn!("Invalid file handle for fstat: {}", handle);
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
    info!("Max read buffer size: {} bytes", args.max_read_size);

    let server_config = Arc::new(ServerConfig {
        username: args.username,
        password: args.password,
        root_dir,
        max_read_size: args.max_read_size,
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
    info!("Use credentials: username='{}', password='***'", server.config.username);

    server
        .run_on_address(
            Arc::new(config),
            (args.host.as_str(), args.port),
        )
        .await
        .unwrap();
}