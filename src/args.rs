use std::path::PathBuf;

use clap::Parser;

/// Configurazione da linea di comando
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Indirizzo IP su cui ascoltare
    #[arg(short, long, default_value = "0.0.0.0")]
    pub host: String,

    /// Porta su cui ascoltare
    #[arg(short, long, default_value = "22")]
    pub port: u16,

    /// Username per autenticazione password
    #[arg(long, default_value = "admin")]
    pub username: String,

    /// Password per autenticazione password
    #[arg(long, default_value = "password")]
    pub password: String,

    /// Directory radice per le operazioni SFTP
    #[arg(long, default_value = ".")]
    pub root_dir: PathBuf,

    /// Dimensione massima del buffer per il download (in bytes)
    #[arg(long, default_value = "32768")]
    pub max_read_size: u32,
}
