use log::{info, warn};
use russh_sftp::protocol::{File, FileAttributes, Name, StatusCode};

use crate::sftp::{SftpSession, utils::metadata::MetadataConverter};

pub async fn hanle_readdir(
    session: &mut SftpSession,
    id: u32,
    handle: String,
) -> Result<Name, StatusCode> {
    info!("readdir handle: {}", handle);

    if let Some(read_dir) = session.state.open_dirs.get_mut(&handle) {
        let mut files = Vec::new();

        // Leggi alcuni file dalla directory
        for _ in 0..10 {
            // Leggi massimo 10 file per volta
            match read_dir.next_entry().await {
                Ok(Some(entry)) => {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    match entry.metadata().await {
                        Ok(metadata) => {
                            //let attrs = Self::metadata_to_file_attributes(&metadata).await;
                            let attrs = MetadataConverter::to_file_attributes(&metadata).await;
                            let longname =
                                MetadataConverter::format_longname(&file_name, &metadata).await;
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
                                longname: format!(
                                    "-rw-r--r-- 1 root root 0 Jan  1 00:00 {}",
                                    file_name
                                ),
                                attrs,
                            });
                        }
                    }
                }
                Ok(None) => break, // Fine directory
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
