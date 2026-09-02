//! Same-directory file transactions shared by converted documents and reports.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commits bytes through a same-directory temporary file.
pub fn commit_bytes(output: &Path, bytes: &[u8], overwrite: bool) -> io::Result<()> {
    if output.exists() && !overwrite {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", output.display()),
        ));
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = unique_sibling(output, "tmp")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        if !overwrite || !output.exists() {
            if !overwrite && output.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("output already exists: {}", output.display()),
                ));
            }
            fs::rename(&temporary, output)?;
            return Ok(());
        }

        let backup = unique_sibling(output, "bak")?;
        fs::rename(output, &backup)?;
        match fs::rename(&temporary, output) {
            Ok(()) => {
                fs::remove_file(backup)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&backup, output);
                Err(error)
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Returns a unique, non-existing sibling path for one transaction artifact.
fn unique_sibling(output: &Path, suffix: &str) -> io::Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("output");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok(parent.join(format!(".{file_name}.{stamp}.{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_no_overwrite_preserves_existing_output() {
        let directory = std::env::temp_dir().join(format!(
            "aseprite-psd-atomic-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        let output = directory.join("existing.bin");
        fs::write(&output, b"original").expect("write original");

        let error = commit_bytes(&output, b"replacement", false)
            .expect_err("no-overwrite transaction must fail");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).expect("read original"), b"original");
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
