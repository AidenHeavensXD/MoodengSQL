use crate::types::Row;
use crate::wal::{WalOp, WriteAheadLog};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Row identifier — monotonically increasing for O(1) lookup.
pub type RowId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TableData {
    rows: HashMap<RowId, Row>,
    next_id: RowId,
}

impl Default for TableData {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            next_id: 1,
        }
    }
}

/// Row store with WAL-backed durability and periodic checkpointing.
#[derive(Debug)]
pub struct StorageEngine {
    data_dir: PathBuf,
    tables: RwLock<HashMap<String, TableData>>,
    wal: Arc<WriteAheadLog>,
}

impl StorageEngine {
    pub fn open(data_dir: impl AsRef<Path>, wal: Arc<WriteAheadLog>) -> crate::error::Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir)?;

        let engine = Self {
            data_dir,
            tables: RwLock::new(HashMap::new()),
            wal,
        };
        engine.load_all()?;
        Ok(engine)
    }

    pub fn wal(&self) -> &Arc<WriteAheadLog> {
        &self.wal
    }

    fn table_path(&self, table: &str) -> PathBuf {
        self.data_dir.join(format!("{table}.dat"))
    }

    fn load_all(&self) -> crate::error::Result<()> {
        if !self.data_dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "dat") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    let data = Self::read_table_file(&path)?;
                    self.tables.write().insert(name, data);
                }
            }
        }
        Ok(())
    }

    fn read_table_file(path: &Path) -> crate::error::Result<TableData> {
        let mut file = File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        if buf.is_empty() {
            return Ok(TableData::default());
        }
        bincode::deserialize(&buf).map_err(|e| crate::error::MoodengError::Storage(e.to_string()))
    }

    fn persist_table(&self, table: &str) -> crate::error::Result<()> {
        let tables = self.tables.read();
        let data = tables
            .get(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let encoded = bincode::serialize(data)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;

        let path = self.table_path(table);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(())
    }

    pub fn checkpoint_all(&self) -> crate::error::Result<()> {
        let names: Vec<String> = self.tables.read().keys().cloned().collect();
        for table in names {
            if self.tables.read().get(&table).is_some() {
                self.persist_table(&table)?;
            }
        }
        Ok(())
    }

    fn maybe_checkpoint(&self) -> crate::error::Result<()> {
        if self.wal.should_checkpoint() {
            crate::recovery::checkpoint(self, &self.wal)?;
        }
        Ok(())
    }

    pub fn ensure_table(&self, table: &str) {
        self.tables
            .write()
            .entry(table.to_string())
            .or_insert_with(TableData::default);
    }

    pub fn insert(&self, table: &str, row: Row, txn_id: u64) -> crate::error::Result<RowId> {
        let mut tables = self.tables.write();
        let data = tables
            .entry(table.to_string())
            .or_insert_with(TableData::default);

        let id = data.next_id;
        data.next_id += 1;
        data.rows.insert(id, row.clone());
        drop(tables);

        self.wal.append(WalOp::Insert {
            txn_id,
            table: table.to_string(),
            row_id: id,
            row,
        })?;
        self.maybe_checkpoint()?;
        Ok(id)
    }

    pub fn apply_insert(
        &self,
        table: &str,
        row_id: RowId,
        row: Row,
        log: bool,
        txn_id: u64,
    ) -> crate::error::Result<()> {
        let mut tables = self.tables.write();
        let data = tables
            .entry(table.to_string())
            .or_insert_with(TableData::default);
        if row_id >= data.next_id {
            data.next_id = row_id + 1;
        }
        data.rows.insert(row_id, row.clone());
        drop(tables);

        if log {
            self.wal.append(WalOp::Insert {
                txn_id,
                table: table.to_string(),
                row_id,
                row,
            })?;
            self.maybe_checkpoint()?;
        }
        Ok(())
    }

    pub fn get(&self, table: &str, id: RowId) -> crate::error::Result<Option<Row>> {
        let tables = self.tables.read();
        Ok(tables
            .get(table)
            .and_then(|t| t.rows.get(&id).cloned()))
    }

    pub fn scan(&self, table: &str) -> crate::error::Result<Vec<(RowId, Row)>> {
        let tables = self.tables.read();
        let Some(data) = tables.get(table) else {
            return Ok(Vec::new());
        };
        let mut rows: Vec<_> = data.rows.iter().map(|(&id, row)| (id, row.clone())).collect();
        rows.sort_by_key(|(id, _)| *id);
        Ok(rows)
    }

    pub fn fetch_by_ids(
        &self,
        table: &str,
        ids: &[RowId],
    ) -> crate::error::Result<Vec<(RowId, Row)>> {
        let tables = self.tables.read();
        let data = tables
            .get(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let mut rows = Vec::new();
        for &id in ids {
            if let Some(row) = data.rows.get(&id) {
                rows.push((id, row.clone()));
            }
        }
        Ok(rows)
    }

    pub fn update(
        &self,
        table: &str,
        id: RowId,
        mut row: Row,
        txn_id: u64,
        expected_version: u64,
    ) -> crate::error::Result<bool> {
        let mut tables = self.tables.write();
        let data = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let Some(existing) = data.rows.get(&id) else {
            return Ok(false);
        };

        if existing.version != expected_version {
            return Err(crate::error::MoodengError::VersionConflict {
                table: table.to_string(),
                row_id: id,
            });
        }

        row.version = expected_version + 1;
        data.rows.insert(id, row.clone());
        drop(tables);

        self.wal.append(WalOp::Update {
            txn_id,
            table: table.to_string(),
            row_id: id,
            row,
        })?;
        self.maybe_checkpoint()?;
        Ok(true)
    }

    pub fn apply_update(
        &self,
        table: &str,
        id: RowId,
        row: Row,
        log: bool,
        txn_id: u64,
    ) -> crate::error::Result<bool> {
        let wal_row = {
            let mut tables = self.tables.write();
            let data = tables
                .get_mut(table)
                .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

            let Some(existing) = data.rows.get(&id) else {
                return Ok(false);
            };

            let mut new_row = row.clone();
            new_row.version = existing.version + 1;
            data.rows.insert(id, new_row.clone());
            if log {
                Some(new_row)
            } else {
                None
            }
        };

        if let Some(new_row) = wal_row {
            self.wal.append(WalOp::Update {
                txn_id,
                table: table.to_string(),
                row_id: id,
                row: new_row,
            })?;
            self.maybe_checkpoint()?;
        }
        Ok(true)
    }

    pub fn delete(&self, table: &str, id: RowId, txn_id: u64) -> crate::error::Result<bool> {
        let mut tables = self.tables.write();
        let data = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        if data.rows.remove(&id).is_some() {
            drop(tables);
            self.wal.append(WalOp::Delete {
                txn_id,
                table: table.to_string(),
                row_id: id,
            })?;
            self.maybe_checkpoint()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn apply_delete(&self, table: &str, id: RowId, log: bool, txn_id: u64) -> crate::error::Result<bool> {
        let mut tables = self.tables.write();
        let data = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let deleted = data.rows.remove(&id).is_some();
        drop(tables);

        if deleted && log {
            self.wal.append(WalOp::Delete {
                txn_id,
                table: table.to_string(),
                row_id: id,
            })?;
            self.maybe_checkpoint()?;
        }
        Ok(deleted)
    }

    pub fn drop_table(&self, table: &str) -> crate::error::Result<()> {
        self.tables.write().remove(table);
        let path = self.table_path(table);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn row_count(&self, table: &str) -> u64 {
        self.tables
            .read()
            .get(table)
            .map(|t| t.rows.len() as u64)
            .unwrap_or(0)
    }
}
