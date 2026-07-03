use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::TableSchema;
use crate::index::BTreeIndex;

const META_VERSION: u32 = 1;
const META_FILE: &str = "meta.bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseSnapshot {
    pub version: u32,
    pub tables: Vec<TableSchema>,
    pub indexes: HashMap<String, HashMap<String, BTreeIndex>>,
}

impl Default for DatabaseSnapshot {
    fn default() -> Self {
        Self {
            version: META_VERSION,
            tables: Vec::new(),
            indexes: HashMap::new(),
        }
    }
}

pub struct MetaStore {
    path: PathBuf,
}

impl MetaStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(META_FILE),
        }
    }

    pub fn load(&self) -> crate::error::Result<Option<DatabaseSnapshot>> {
        if !self.path.exists() {
            return Ok(None);
        }

        let mut file = File::open(&self.path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        if buf.is_empty() {
            return Ok(None);
        }

        let snapshot: DatabaseSnapshot = bincode::deserialize(&buf)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;

        if snapshot.version != META_VERSION {
            return Err(crate::error::MoodengError::Storage(format!(
                "unsupported meta version {} (expected {META_VERSION})",
                snapshot.version
            )));
        }

        Ok(Some(snapshot))
    }

    pub fn save(&self, snapshot: &DatabaseSnapshot) -> crate::error::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let encoded = bincode::serialize(snapshot)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;

        let path = &self.path;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(())
    }
}

pub fn reconcile_storage(
    data_dir: &Path,
    catalog_tables: &[String],
) -> crate::error::Result<Vec<String>> {
    let mut dat_tables = Vec::new();
    if data_dir.exists() {
        for entry in fs::read_dir(data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "dat") {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    dat_tables.push(name.to_string());
                }
            }
        }
    }

    let mut warnings = Vec::new();
    for table in &dat_tables {
        if !catalog_tables.iter().any(|t| t == table) {
            warnings.push(format!(
                "orphan data file '{table}.dat' has no schema in catalog"
            ));
        }
    }

    Ok(warnings)
}
