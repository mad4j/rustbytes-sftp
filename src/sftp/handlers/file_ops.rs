use log::{error, info, warn};
use russh_sftp::protocol::{Data, StatusCode};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::sftp::SftpSession;

pub async fn handle_read(
    session: &mut SftpSession,
    id: u32,
    handle: String,
    offset: u64,
    len: u32,
) -> Result<Data, StatusCode> {
    info!(
        "read handle: {}, offset: {}, requested len: {}",
        handle, offset, len
    );

    if let Some(open_file) = session.state.open_files.get_mut(&handle) {
        // Limita la dimensione della lettura al massimo configurato
        let actual_len = std::cmp::min(len, session.state.max_read_size);

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

                            info!(
                                "Successfully read {} bytes from handle: {} (binary: {}, offset: {})",
                                bytes_read, handle, open_file.is_binary, offset
                            );

                            // Log aggiuntivo per file di testo (primi 100 caratteri se non binario)
                            if !open_file.is_binary && bytes_read > 0 {
                                let preview = String::from_utf8_lossy(
                                    &buffer[..std::cmp::min(100, bytes_read)],
                                );
                                info!(
                                    "Text file preview: {}",
                                    preview.chars().take(50).collect::<String>()
                                );
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
