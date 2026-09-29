#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::{fs, io};

/// A uniquely named temporary directory that is deleted on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> io::Result<Self> {
        let mut name = String::from(prefix);
        name.push_str(&format!(
            "-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let path = std::env::temp_dir().join(name);
        fs::create_dir_all(&path)?;
        Ok(TempDir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, relative: &str, contents: &str) -> io::Result<PathBuf> {
        let full = self.path.join(relative);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full, contents)?;
        Ok(full)
    }

    pub fn mkdir(&self, relative: &str) -> io::Result<PathBuf> {
        let full = self.path.join(relative);
        fs::create_dir_all(&full)?;
        Ok(full)
    }

    pub fn symlink_file(&self, target: &Path, link: &str) -> io::Result<()> {
        #[cfg(windows)]
        {
            if std::env::var_os("MD_DISCOVER_SYMLINKS").is_none() {
                // Symlink creation needs privileges or Developer Mode on
                // Windows; the caller checks the returned error and skips.
            }
        }
        let link_path = self.path.join(link);
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)?;
        }
        symlink(target, &link_path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
