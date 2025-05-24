use std::{collections::HashMap, sync::Arc};

use log::{error, info, warn};
use russh_sftp::protocol::{
    Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::{
    fs,
    io::{AsyncSeekExt, AsyncWriteExt},
};

use crate::{file_info::FileInfo, server::ServerConfig, sftp::utils::metadata::MetadataConverter};

use super::{
    SessionState,
    handlers::{self, file_ops},
    utils::path_resolver::PathResolver,
};

pub struct SftpSession {
    pub(crate) state: SessionState,
    pub(crate) path_resolver: PathResolver,
}

impl SftpSession {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            state: SessionState {
                version: None,
                _root_dir: config.root_dir.clone(),
                open_files: HashMap::new(),
                open_dirs: HashMap::new(),
                handle_counter: 0,
                max_read_size: config.max_read_size,
            },
            path_resolver: PathResolver::new(config.root_dir.clone()),
        }
    }

    fn next_handle(&mut self) -> String {
        self.state.handle_counter += 1;
        format!("handle_{}", self.state.handle_counter)
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
        if self.state.version.is_some() {
            error!("duplicate SSH_FXP_VERSION packet");
            return Err(StatusCode::ConnectionLost);
        }

        self.state.version = Some(version);
        info!(
            "version: {:?}, extensions: {:?}",
            self.state.version, extensions
        );
        Ok(Version::new())
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        info!("close handle: {}", handle);

        // Rimuovi il file o directory dal tracking
        if let Some(open_file) = self.state.open_files.remove(&handle) {
            info!(
                "Closed file handle: {} (path: {:?}, binary: {})",
                handle, open_file.path, open_file.is_binary
            );
        } else if self.state.open_dirs.remove(&handle).is_some() {
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

        let resolved_path = self.path_resolver.resolve_path(&path)?;

        match fs::read_dir(&resolved_path).await {
            Ok(read_dir) => {
                let handle = self.next_handle();
                self.state.open_dirs.insert(handle.clone(), read_dir);
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
        handlers::dir_ops::hanle_readdir(self, id, handle).await
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        info!("realpath: {}", path);

        let resolved_path = self.path_resolver.resolve_path(&path)?;

        match resolved_path.canonicalize() {
            Ok(canonical) => {
                // Converti il path canonico in un path relativo alla root
                let relative_path =
                    if let Ok(rel) = canonical.strip_prefix(&self.path_resolver.get_root_dir()) {
                        format!("/{}", rel.to_string_lossy())
                    } else {
                        "/".to_string()
                    };

                Ok(Name {
                    id,
                    files: vec![File::dummy(&relative_path)],
                })
            }
            Err(_) => Ok(Name {
                id,
                files: vec![File::dummy("/")],
            }),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        path: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        info!("open file: {} with flags: {:?}", path, pflags);

        let resolved_path = self.path_resolver.resolve_path(&path)?;

        // Determina se è un'operazione di scrittura
        let is_write = pflags.intersects(
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::APPEND,
        );

        if is_write {
            // Per operazioni di scrittura, assicurati che la directory parent esista
            if let Some(parent) = resolved_path.parent() {
                if !parent.exists() {
                    warn!("Parent directory does not exist: {:?}", parent);
                    return Err(StatusCode::NoSuchFile);
                }
            }

            // Crea o apri il file per scrittura
            let open_options = {
                let mut opts = fs::OpenOptions::new();

                if pflags.contains(OpenFlags::READ) {
                    opts.read(true);
                }

                if pflags.contains(OpenFlags::WRITE) {
                    opts.write(true);
                }

                if pflags.contains(OpenFlags::CREATE) {
                    opts.create(true);
                }

                if pflags.contains(OpenFlags::TRUNCATE) {
                    opts.truncate(true);
                }

                if pflags.contains(OpenFlags::APPEND) {
                    opts.append(true);
                }

                opts
            };

            match open_options.open(&resolved_path).await {
                Ok(file) => match FileInfo::from_file(file, resolved_path).await {
                    Ok(open_file) => {
                        let handle = self.next_handle();
                        info!(
                            "Successfully opened file for write with handle: {} (binary: {})",
                            handle, open_file.is_binary
                        );
                        self.state.open_files.insert(handle.clone(), open_file);
                        Ok(Handle { id, handle })
                    }
                    Err(e) => {
                        warn!("Failed to create FileInfo: {}", e);
                        Err(StatusCode::Failure)
                    }
                },
                Err(e) => {
                    warn!("Failed to open/create file {:?}: {}", resolved_path, e);
                    match e.kind() {
                        std::io::ErrorKind::NotFound => Err(StatusCode::NoSuchFile),
                        std::io::ErrorKind::PermissionDenied => Err(StatusCode::PermissionDenied),
                        std::io::ErrorKind::AlreadyExists => Err(StatusCode::Failure),
                        _ => Err(StatusCode::Failure),
                    }
                }
            }
        } else {
            // Per operazioni di lettura, il file deve esistere
            if !resolved_path.exists() {
                warn!("File does not exist: {:?}", resolved_path);
                return Err(StatusCode::NoSuchFile);
            }

            // Controlla se è un file regolare
            if !resolved_path.is_file() {
                warn!("Path is not a regular file: {:?}", resolved_path);
                return Err(StatusCode::Failure);
            }

            match FileInfo::new(resolved_path).await {
                Ok(open_file) => {
                    let handle = self.next_handle();
                    info!(
                        "Successfully opened file for read with handle: {} (binary: {})",
                        handle, open_file.is_binary
                    );
                    self.state.open_files.insert(handle.clone(), open_file);
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
    }

    /// Implementazione migliorata del comando READ per supportare download di file di testo e binari
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        file_ops::handle_read(self, id, handle, offset, len).await
    }

    /// Implementazione del comando WRITE per supportare upload di file
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        info!(
            "write handle: {}, offset: {}, data len: {}",
            handle,
            offset,
            data.len()
        );

        if let Some(open_file) = self.state.open_files.get_mut(&handle) {
            match open_file.file.seek(std::io::SeekFrom::Start(offset)).await {
                Ok(actual_offset) => {
                    if actual_offset != offset {
                        warn!("Seek to {} resulted in position {}", offset, actual_offset);
                    }

                    match open_file.file.write_all(&data).await {
                        Ok(_) => {
                            // Assicurati che i dati siano scritti su disco
                            if let Err(e) = open_file.file.flush().await {
                                warn!("Failed to flush file handle {}: {}", handle, e);
                            }

                            info!(
                                "Successfully wrote {} bytes to handle: {} at offset: {}",
                                data.len(),
                                handle,
                                offset
                            );

                            Ok(Status {
                                id,
                                status_code: StatusCode::Ok,
                                error_message: "Ok".to_string(),
                                language_tag: "en-US".to_string(),
                            })
                        }
                        Err(e) => {
                            error!("Failed to write to file handle {}: {}", handle, e);
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
            warn!("Invalid file handle for write: {}", handle);
            Err(StatusCode::BadMessage)
        }
    }

    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("stat: {}", path);

        let resolved_path = self.path_resolver.resolve_path(&path)?;

        match fs::metadata(&resolved_path).await {
            Ok(metadata) => {
                let attrs = MetadataConverter::to_file_attributes(&metadata).await;
                info!(
                    "stat result for {:?}: size={:?}, is_file={}",
                    resolved_path,
                    attrs.size,
                    metadata.is_file()
                );
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

    async fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("lstat: {}", path);

        let resolved_path = self.path_resolver.resolve_path(&path)?;

        match fs::symlink_metadata(&resolved_path).await {
            Ok(metadata) => {
                let attrs = MetadataConverter::to_file_attributes(&metadata).await;
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

    async fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        info!("fstat handle: {}", handle);

        if let Some(open_file) = self.state.open_files.get(&handle) {
            match open_file.file.metadata().await {
                Ok(metadata) => {
                    let attrs = MetadataConverter::to_file_attributes(&metadata).await;
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
