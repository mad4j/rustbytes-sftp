use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use crate::file_info::FileInfo;

pub type HandleId = String;
pub type OpenFiles = HashMap<HandleId, FileInfo>;
pub type OpenDirs = HashMap<HandleId, fs::ReadDir>;

pub struct SessionState {
    pub version: Option<u32>,
    pub root_dir: PathBuf,
    pub open_files: OpenFiles,
    pub open_dirs: OpenDirs,
    pub handle_counter: u32,
    pub max_read_size: u32,
}