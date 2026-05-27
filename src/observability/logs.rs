use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use crate::runtime::validation::validate_service_name;

#[derive(Debug, Clone)]
pub struct LogStore {
    dir: PathBuf,
}

impl LogStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    pub fn append(&self, service_name: &str, line: &str) -> std::io::Result<()> {
        validate_service_name(service_name)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join(format!("{service_name}.log")))?;
        file.write_all(line.as_bytes())
    }

    pub fn read_recent(&self, service_name: &str, limit: usize) -> std::io::Result<Vec<String>> {
        validate_service_name(service_name)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let mut content = String::new();
        OpenOptions::new()
            .read(true)
            .open(self.dir.join(format!("{service_name}.log")))?
            .read_to_string(&mut content)?;
        let lines = content
            .lines()
            .rev()
            .take(limit)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        Ok(lines.into_iter().rev().collect())
    }
}

/// Splits a buffer into complete lines (newline-terminated) and any trailing
/// partial line (text after the last `\n`).
fn split_complete(buf: &str) -> (Vec<String>, String) {
    match buf.rfind('\n') {
        Some(idx) => {
            let complete = buf[..idx].split('\n').map(str::to_string).collect();
            let partial = buf[idx + 1..].to_string();
            (complete, partial)
        }
        None => (Vec::new(), buf.to_string()),
    }
}

/// Follows a single log file: produces an initial backlog, then returns only
/// newly-appended complete lines on each `poll`. Synchronous; safe to call from
/// a spawned task that owns it.
#[derive(Debug)]
pub struct LogTailer {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl LogTailer {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            offset: 0,
            partial: String::new(),
        }
    }

    /// Returns the last `limit` complete lines and advances the read position to
    /// end-of-file. A trailing partial line (no final newline) is buffered, not
    /// returned. Missing file yields an empty backlog.
    pub fn backlog(&mut self, limit: usize) -> std::io::Result<Vec<String>> {
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.offset = 0;
                self.partial.clear();
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        self.offset = bytes.len() as u64;
        let text = String::from_utf8_lossy(&bytes);
        let (complete, partial) = split_complete(&text);
        self.partial = partial;
        let start = complete.len().saturating_sub(limit);
        Ok(complete[start..].to_vec())
    }

    /// Returns complete lines appended since the last call. Buffers any trailing
    /// partial line until it is newline-terminated. Resets to start of file if
    /// the file shrank (truncation/recreation). Missing file yields empty.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        self.offset = len;

        let mut combined = std::mem::take(&mut self.partial);
        combined.push_str(&String::from_utf8_lossy(&bytes));
        let (complete, partial) = split_complete(&combined);
        self.partial = partial;
        Ok(complete)
    }
}

#[cfg(test)]
mod tailer_tests {
    use super::LogTailer;
    use std::fs;
    use std::io::Write;

    fn write(path: &std::path::Path, contents: &str) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn append(path: &std::path::Path, contents: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn backlog_returns_last_n_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "l1\nl2\nl3\n");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(2).unwrap(), vec!["l2", "l3"]);
    }

    #[test]
    fn backlog_buffers_trailing_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\nb");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), vec!["a"]);
        // "b" is buffered, not yet a complete line; completed on next poll
        append(&path, "c\n");
        assert_eq!(tailer.poll().unwrap(), vec!["bc"]);
    }

    #[test]
    fn poll_returns_new_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\n");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), vec!["a"]);
        append(&path, "b\nc\n");
        assert_eq!(tailer.poll().unwrap(), vec!["b", "c"]);
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn poll_buffers_partial_until_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\n");
        let mut tailer = LogTailer::new(&path);
        let _ = tailer.backlog(10).unwrap();
        append(&path, "par");
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
        append(&path, "tial\n");
        assert_eq!(tailer.poll().unwrap(), vec!["partial"]);
    }

    #[test]
    fn poll_resets_on_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\nb\n");
        let mut tailer = LogTailer::new(&path);
        let _ = tailer.backlog(10).unwrap();
        // recreate smaller (len < offset) -> tailer re-reads from start
        write(&path, "x\n");
        assert_eq!(tailer.poll().unwrap(), vec!["x"]);
    }

    #[test]
    fn missing_file_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.log");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), Vec::<String>::new());
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
    }
}
