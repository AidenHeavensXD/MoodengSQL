use std::fs::{self, File};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder};

use crate::recovery::checkpoint;
use crate::Database;

/// Create a gzip-compressed tar archive of the database data directory.
pub fn backup(data_dir: impl AsRef<Path>, output: impl AsRef<Path>) -> crate::error::Result<()> {
    let data_dir = data_dir.as_ref();
    let output = output.as_ref();

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(encoder);

    if data_dir.exists() {
        for entry in fs::read_dir(data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file");
                tar.append_path_with_name(&path, name).map_err(|e| {
                    crate::error::MoodengError::Storage(format!("backup tar: {e}"))
                })?;
            }
        }
    }

    let inner = tar.into_inner().map_err(|e| {
        crate::error::MoodengError::Storage(format!("backup finish: {e}"))
    })?;
    inner.finish().map_err(|e| {
        crate::error::MoodengError::Storage(format!("backup gzip: {e}"))
    })?;
    Ok(())
}

/// Backup with checkpoint — flushes WAL and metadata before archiving.
pub fn backup_live(db: &Database, output: impl AsRef<Path>) -> crate::error::Result<()> {
    db.checkpoint()?;
    backup(db.data_dir(), output)
}

/// Restore a backup archive into a data directory (overwrites existing files).
pub fn restore(data_dir: impl AsRef<Path>, archive: impl AsRef<Path>) -> crate::error::Result<()> {
    let data_dir = data_dir.as_ref();
    let archive = archive.as_ref();

    fs::create_dir_all(data_dir)?;

    let file = File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    archive.unpack(data_dir).map_err(|e| {
        crate::error::MoodengError::Storage(format!("restore: {e}"))
    })?;

    Ok(())
}

impl Database {
    /// Flush WAL, persist metadata, and checkpoint table files.
    pub fn checkpoint(&self) -> crate::error::Result<()> {
        checkpoint(&self.storage, self.wal())?;
        self.persist()
    }
}

/// List files included in a backup (for verification).
pub fn list_backup_files(archive: impl AsRef<Path>) -> crate::error::Result<Vec<String>> {
    let file = File::open(archive.as_ref())?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut names = Vec::new();

    for entry in archive.entries().map_err(|e| {
        crate::error::MoodengError::Storage(format!("list backup: {e}"))
    })? {
        let entry = entry.map_err(|e| crate::error::MoodengError::Storage(format!("{e}")))?;
        if let Ok(path) = entry.path() {
            names.push(path.display().to_string());
        }
    }
    Ok(names)
}
