use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

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
        fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{service_name}.log")))?;
        file.write_all(line.as_bytes())
    }

    pub fn read_recent(&self, service_name: &str, limit: usize) -> std::io::Result<Vec<String>> {
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
