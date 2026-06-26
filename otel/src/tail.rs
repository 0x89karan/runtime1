use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Tracks (device, inode, offset) to detect log rotation.
pub struct FileTailer {
    path: PathBuf,
    dev: u64,
    ino: u64,
    offset: u64,
}

impl FileTailer {
    /// Open the file for tailing. If `from_beginning` is true, tail from byte 0;
    /// otherwise seek to EOF (same semantics as `tail -f`).
    pub async fn open(path: PathBuf, from_beginning: bool) -> anyhow::Result<Self> {
        let meta = tokio::fs::metadata(&path).await?;
        let dev = meta.dev();
        let ino = meta.ino();
        let offset = if from_beginning { 0 } else { meta.len() };
        Ok(Self { path, dev, ino, offset })
    }

    /// Poll for new lines since the last call. Returns (lines, rotated).
    /// `rotated` is true when the file was replaced (inode changed or
    /// copy-truncate: same inode but file is shorter than remembered offset).
    pub async fn poll(&mut self) -> anyhow::Result<(Vec<String>, bool)> {
        let meta = match tokio::fs::metadata(&self.path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((vec![], false));
            }
            Err(e) => return Err(e.into()),
        };

        let cur_dev = meta.dev();
        let cur_ino = meta.ino();
        let cur_len = meta.len();

        // Rotation detection: new inode (logrotate rename) or copy-truncate.
        let rotated = cur_dev != self.dev
            || cur_ino != self.ino
            || cur_len < self.offset;

        if rotated {
            self.dev = cur_dev;
            self.ino = cur_ino;
            self.offset = 0;
        }

        if cur_len <= self.offset {
            return Ok((vec![], rotated));
        }

        let file = tokio::fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(file);

        // Seek to remembered offset.
        if self.offset > 0 {
            use tokio::io::AsyncSeekExt;
            reader.seek(std::io::SeekFrom::Start(self.offset)).await?;
        }

        let mut lines = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
            if !trimmed.is_empty() {
                lines.push(trimmed.to_owned());
            }
        }

        // Update offset to the current end of consumed data.
        use tokio::io::AsyncSeekExt;
        self.offset = reader.seek(std::io::SeekFrom::Current(0)).await?;

        Ok((lines, rotated))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn test_tail_from_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"kind":"agent_spawned"}}"#).unwrap();
            writeln!(f, r#"{{"kind":"inference_request"}}"#).unwrap();
        }
        let mut tailer = FileTailer::open(path.clone(), true).await.unwrap();
        let (lines, rotated) = tailer.poll().await.unwrap();
        assert!(!rotated);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("agent_spawned"));
    }

    #[tokio::test]
    async fn test_tail_from_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"kind":"old_event"}}"#).unwrap();
        }
        let mut tailer = FileTailer::open(path.clone(), false).await.unwrap();
        // Nothing new yet
        let (lines, _) = tailer.poll().await.unwrap();
        assert!(lines.is_empty());
        // Append a new line
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, r#"{{"kind":"new_event"}}"#).unwrap();
        }
        let (lines, _) = tailer.poll().await.unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("new_event"));
    }

    #[tokio::test]
    async fn test_tail_copy_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");

        // Write a large block so offset > 0 after poll
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for _ in 0..10 {
                writeln!(f, r#"{{"kind":"original"}}"#).unwrap();
            }
        }
        let mut tailer = FileTailer::open(path.clone(), true).await.unwrap();
        let (first, _) = tailer.poll().await.unwrap();
        assert_eq!(first.len(), 10); // consumed all 10 lines; offset = file size

        // copy-truncate: file is truncated to 0 (same inode), then small data written
        // At poll time, cur_len < self.offset → rotation detected
        std::fs::File::create(&path).unwrap(); // truncate to 0
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, r#"{{"kind":"a"}}"#).unwrap(); // tiny line, stays < old offset
        }
        let (lines, rotated) = tailer.poll().await.unwrap();
        assert!(rotated, "should detect truncation when cur_len < old offset");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"a\""));
    }
}
