use log::warn;
use russh_sftp::protocol::StatusCode;
use std::path::{Path, PathBuf};

pub struct PathResolver {
    root_dir: PathBuf,
}

impl PathResolver {
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    pub fn resolve_path(&self, path: &str) -> Result<PathBuf, StatusCode> {
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
                    warn!(
                        "Tentativo di accesso fuori dalla root directory: {:?}",
                        canonical
                    );
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
                                warn!(
                                    "Tentativo di accesso fuori dalla root directory: {:?}",
                                    resolved
                                );
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

    pub fn get_root_dir(&self) -> &PathBuf {
        &self.root_dir
    }
}
