use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};

/// Bytes of file content stored at the last-consumed position.
/// Used to detect copy-truncate rotation when the new file grows past the old offset
/// before the next poll tick (making the length-regression check insufficient).
const SENTINEL_SIZE: usize = 64;

/// Tracks (device, inode, offset, sentinel) to detect log rotation.
pub struct FileTailer {
    path: PathBuf,
    dev: u64,
    ino: u64,
    offset: u64,
    /// Last SENTINEL_SIZE bytes at self.offset. Empty until the first successful capture.
    last_sentinel: Vec<u8>,
}

impl FileTailer {
    /// Open the file for tailing. If `from_beginning` is true, tail from byte 0;
    /// otherwise seek to EOF (same semantics as `tail -f`).
    pub async fn open(path: PathBuf, from_beginning: bool) -> anyhow::Result<Self> {
        let meta = tokio::fs::metadata(&path).await?;
        let dev = meta.dev();
        let ino = meta.ino();
        let offset = if from_beginning { 0 } else { meta.len() };
        Ok(Self { path, dev, ino, offset, last_sentinel: vec![] })
    }

    /// Poll for new lines since the last call. Returns (lines, rotated).
    /// `rotated` is true when the file was replaced (inode changed, copy-truncate
    /// length regression, or content sentinel mismatch).
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

        // Rotation detection: new inode (logrotate rename) or copy-truncate length regression.
        let mut rotated = cur_dev != self.dev
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

        // Content-sentinel check: detect copy-truncate when the new file has grown past
        // self.offset before the next poll (making cur_len >= self.offset a false negative).
        // Guards:
        //   - already rotated via inode/length check → skip
        //   - self.offset < SENTINEL_SIZE → no sentinel bytes to compare (also prevents u64 underflow)
        //   - last_sentinel not yet populated (len != SENTINEL_SIZE) → skip to avoid
        //     false-positive on the first append after a from_beginning=false start
        if !rotated
            && self.offset >= SENTINEL_SIZE as u64
            && self.last_sentinel.len() == SENTINEL_SIZE
        {
            reader.seek(std::io::SeekFrom::Start(self.offset - SENTINEL_SIZE as u64)).await?;
            let mut buf = [0u8; SENTINEL_SIZE];
            match reader.read_exact(&mut buf).await {
                Ok(_) if buf != self.last_sentinel.as_slice() => {
                    // File content changed at the sentinel window → copy-truncate rotation.
                    rotated = true;
                    self.dev = cur_dev;
                    self.ino = cur_ino;
                    self.offset = 0;
                }
                Ok(_) => {} // Sentinel matches — normal append, no rotation.
                Err(_) => {
                    // Race: file shrank between metadata and read. Treat as rotation.
                    rotated = true;
                    self.dev = cur_dev;
                    self.ino = cur_ino;
                    self.offset = 0;
                }
            }
        }

        // Clear sentinel whenever rotation is confirmed so stale bytes from the old file
        // cannot contaminate the next check after the new file is partially populated.
        if rotated {
            self.last_sentinel.clear();
        }

        // Seek to the read position (0 if rotated, else self.offset).
        reader.seek(std::io::SeekFrom::Start(self.offset)).await?;

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
        let new_offset = reader.seek(std::io::SeekFrom::Current(0)).await?;
        self.offset = new_offset;

        // Capture sentinel for the next poll.
        // Skip when new_offset < SENTINEL_SIZE (prevents u64 underflow; check guard handles this).
        if new_offset >= SENTINEL_SIZE as u64 {
            reader.seek(std::io::SeekFrom::Start(new_offset - SENTINEL_SIZE as u64)).await?;
            let mut sentinel_buf = vec![0u8; SENTINEL_SIZE];
            if reader.read_exact(&mut sentinel_buf).await.is_ok() {
                self.last_sentinel = sentinel_buf;
            }
            // On read_exact failure (race), keep old sentinel; the len guard prevents false positives.
        }

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

    /// Sentinel: fast-grow copy-truncate — new file grows past old offset before poll.
    /// This is the case the length-regression check misses; the sentinel catches it.
    #[tokio::test]
    async fn test_tail_copy_truncate_fast_grow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");

        // Write enough data that offset will be >= SENTINEL_SIZE after first poll.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..50u32 {
                writeln!(f, r#"{{"kind":"original","seq":{i},"pad":"aaaaaaaaaaaaaaaaaaaaaaaaa"}}"#).unwrap();
            }
        }
        let mut tailer = FileTailer::open(path.clone(), true).await.unwrap();
        let (first, _) = tailer.poll().await.unwrap();
        assert_eq!(first.len(), 50);
        let old_offset = tailer.offset;
        assert!(old_offset >= SENTINEL_SIZE as u64, "offset must be >= SENTINEL_SIZE for sentinel to be captured");
        assert_eq!(tailer.last_sentinel.len(), SENTINEL_SIZE, "sentinel must be populated after poll");

        // Simulate fast-grow copy-truncate: truncate (same inode) then write different
        // content that grows past old_offset before the next poll.
        std::fs::File::create(&path).unwrap(); // truncate to 0
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            for i in 0..60u32 {
                writeln!(f, r#"{{"kind":"rotated","seq":{i},"pad":"bbbbbbbbbbbbbbbbbbbbbbbb"}}"#).unwrap();
            }
        }
        // Verify the new file is larger than old_offset — length check alone would NOT detect rotation.
        let new_len = std::fs::metadata(&path).unwrap().len();
        assert!(new_len >= old_offset, "new file must be >= old_offset to exercise the sentinel path");

        let (lines, rotated) = tailer.poll().await.unwrap();
        assert!(rotated, "sentinel should detect content mismatch despite cur_len >= old_offset");
        assert!(!lines.is_empty(), "should read lines from new file starting at byte 0");
        assert!(lines[0].contains("\"rotated\""), "lines should be from the new file");
    }

    /// Sentinel: normal append after sentinel is captured must NOT trigger false-positive rotation.
    #[tokio::test]
    async fn test_tail_sentinel_no_false_positive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");

        // Write a large initial block so sentinel is captured.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..50u32 {
                writeln!(f, r#"{{"kind":"original","seq":{i},"pad":"aaaaaaaaaaaaaaaaaaaaaaaaa"}}"#).unwrap();
            }
        }
        let mut tailer = FileTailer::open(path.clone(), true).await.unwrap();
        let (first, rotated) = tailer.poll().await.unwrap();
        assert!(!rotated);
        assert_eq!(first.len(), 50);
        assert_eq!(tailer.last_sentinel.len(), SENTINEL_SIZE, "sentinel must be populated");

        // Normal append — add a few lines without truncating.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, r#"{{"kind":"appended","seq":50}}"#).unwrap();
            writeln!(f, r#"{{"kind":"appended","seq":51}}"#).unwrap();
        }

        let (lines, rotated) = tailer.poll().await.unwrap();
        assert!(!rotated, "normal append must not trigger false-positive rotation");
        assert_eq!(lines.len(), 2, "should read only the two new lines");
        assert!(lines[0].contains("\"appended\""));
    }

    /// Sentinel: first append after from_beginning=false start must not cause spurious rotation.
    /// last_sentinel starts as [] (len != SENTINEL_SIZE) so the check is skipped.
    #[tokio::test]
    async fn test_tail_startup_no_false_positive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flight.jsonl");

        // Pre-existing large file — offset set to EOF on open, sentinel never populated.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..50u32 {
                writeln!(f, r#"{{"kind":"existing","seq":{i},"pad":"cccccccccccccccccccccccc"}}"#).unwrap();
            }
        }
        let mut tailer = FileTailer::open(path.clone(), false).await.unwrap();
        assert!(tailer.offset >= SENTINEL_SIZE as u64, "offset must be large enough to trigger check if sentinel were populated");
        assert!(tailer.last_sentinel.is_empty(), "sentinel should be empty on open");

        // First append after startup.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, r#"{{"kind":"new_event"}}"#).unwrap();
        }

        let (lines, rotated) = tailer.poll().await.unwrap();
        assert!(!rotated, "first append after startup must not trigger false-positive rotation");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"new_event\""));
    }
}
