use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use crate::SshSession;


#[derive(Clone)]
pub struct Server {
    pub config: Arc<ServerConfig>,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub username: String,
    pub password: String,
    pub root_dir: PathBuf,
    pub max_read_size: u32,
}

impl russh::server::Server for Server {
    type Handler = SshSession;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        SshSession::new(self.config.clone())
    }
}
