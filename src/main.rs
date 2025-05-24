use args::Args;
use clap::Parser;
use log::{LevelFilter, error, info};
use russh::keys::ssh_key::rand_core::OsRng;
use russh::server::Server as _;
use server::{Server, ServerConfig};
use std::sync::Arc;
use std::time::Duration;

mod args;
mod file_info;
mod server;
mod sftp_session;
mod ssh_session;

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
            error!(
                "Failed to canonicalize root directory {:?}: {}",
                args.root_dir, e
            );
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
    info!(
        "Use credentials: username='{}', password='***'",
        server.config.username
    );

    server
        .run_on_address(Arc::new(config), (args.host.as_str(), args.port))
        .await
        .unwrap();
}
