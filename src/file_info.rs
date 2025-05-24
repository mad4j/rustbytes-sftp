use std::path::{Path, PathBuf};

use log::info;
use tokio::{fs, io::AsyncReadExt};

#[derive(Debug)]
pub struct FileInfo {
    pub file: tokio::fs::File,
    pub path: PathBuf,
    pub is_binary: bool,
}

impl FileInfo {
    pub async fn new(path: PathBuf) -> Result<Self, std::io::Error> {
        let file = fs::File::open(&path).await?;
        let is_binary = Self::detect_binary_file(&path).await;

        info!("Opened file: {:?}, binary: {}", path, is_binary);

        Ok(Self {
            file,
            path,
            is_binary,
        })
    }


    // Nuovo metodo per creare da un file già aperto
    pub async fn from_file(file: fs::File, path: PathBuf) -> Result<Self, std::io::Error> {
        let is_binary = Self::detect_binary_file(&path).await;
        
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
                "exe", "dll", "so", "dylib", "bin", "dat", "db", "sqlite", "sqlite3", "jpg",
                "jpeg", "png", "gif", "bmp", "tiff", "webp", "ico", "mp3", "wav", "ogg", "flac",
                "aac", "m4a", "mp4", "avi", "mkv", "mov", "wmv", "flv", "webm", "pdf", "doc",
                "docx", "xls", "xlsx", "ppt", "pptx", "zip", "rar", "7z", "tar", "gz", "bz2", "xz",
                "obj", "lib", "a", "deb", "rpm", "dmg", "iso",
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
