use russh_sftp::protocol::FileAttributes;

pub struct MetadataConverter;

impl MetadataConverter {
    pub async fn to_file_attributes(metadata: &std::fs::Metadata) -> FileAttributes {
        let mut attrs = FileAttributes::default();
        attrs.size = Some(metadata.len());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            attrs.uid = Some(metadata.uid());
            attrs.gid = Some(metadata.gid());

            let mut mode = metadata.mode();
            if metadata.is_file() {
                mode |= 0o100000; // S_IFREG
            } else if metadata.is_dir() {
                mode |= 0o040000; // S_IFDIR
            } else if metadata.file_type().is_symlink() {
                mode |= 0o120000; // S_IFLNK
            }
            attrs.permissions = Some(mode);
        }

        #[cfg(windows)]
        {
            let mut mode = 0o644;
            if metadata.is_dir() {
                mode = 0o755 | 0o040000;
            } else if metadata.is_file() {
                mode = 0o644 | 0o100000;
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

    pub async fn format_longname(filename: &str, metadata: &std::fs::Metadata) -> String {
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

            format!(
                "{} {:3} {:5} {:5} {:8} {} {}",
                permissions, nlink, uid, gid, size, mtime, filename
            )
        }

        #[cfg(windows)]
        {
            let permissions = if metadata.is_dir() {
                "drwxr-xr-x"
            } else {
                "-rw-r--r--"
            };

            let size = metadata.len();
            format!(
                "{} 1 root root {:8} Jan  1 00:00 {}",
                permissions, size, filename
            )
        }
    }
}