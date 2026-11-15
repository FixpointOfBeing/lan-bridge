use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, Write},
    path::Path,
};

pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// A single log file: truncate at startup and again before exceeding the cap.
/// No rotated files or previous sessions are retained.
pub struct CappedLog {
    file: File,
    size: u64,
    limit: u64,
}

impl CappedLog {
    pub fn open(path: &Path, limit: u64) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("缺少日志目录"))?;
        std::fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: options.open(path)?,
            size: 0,
            limit,
        })
    }
}

impl Write for CappedLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "日志条目超过大小限制",
            ));
        }
        if self.size + bytes.len() as u64 > self.limit {
            self.file.set_len(0)?;
            self.file.rewind()?;
            self.size = 0;
        }
        let written = self.file.write(bytes)?;
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn truncates_on_start_and_at_the_size_limit_without_backups() {
        let state = crate::tests::TestState::new().await;
        let path = state.0.dir.join("server.log");
        std::fs::write(&path, "old session").unwrap();
        let mut log = CappedLog::open(&path, 10).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        log.write_all(b"12345678").unwrap();
        log.write_all(b"abc").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        log.write_all(b"1234567").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10);
        assert!(log.write_all(b"oversized record").is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10);
        drop(log);
        let _log = CappedLog::open(&path, 10).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(std::fs::read_dir(&state.0.dir).unwrap().count(), 2);
    }
}
