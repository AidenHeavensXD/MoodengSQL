use crate::page_table::{PageTable, DEFAULT_ROWS_PER_PAGE};
use crate::types::Row;
use crate::wal::{WalOp, WriteAheadLog};
use parking_lot::{Mutex, RwLock};
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

enum TableBackend {
    Memory(TableData),
    Paged(Mutex<PageTable>),
}

impl TableBackend {
    fn insert(&mut self, row: Row) -> crate::error::Result<RowId> {
        match self {
            TableBackend::Memory(data) => {
                let id = data.next_id;
                data.next_id += 1;
                data.rows.insert(id, row);
                Ok(id)
            }
            TableBackend::Paged(pt) => pt.lock().insert(row),
        }
    }

    fn apply_insert(&mut self, row_id: RowId, row: Row) -> crate::error::Result<()> {
        match self {
            TableBackend::Memory(data) => {
                if row_id >= data.next_id {
                    data.next_id = row_id + 1;
                }
                data.rows.insert(row_id, row);
                Ok(())
            }
            TableBackend::Paged(pt) => pt.lock().apply_insert(row_id, row),
        }
    }

    fn get(&self, id: RowId) -> crate::error::Result<Option<Row>> {
        match self {
            TableBackend::Memory(data) => Ok(data.rows.get(&id).cloned()),
            TableBackend::Paged(pt) => pt.lock().get(id),
        }
    }

    fn scan(&self) -> crate::error::Result<Vec<(RowId, Row)>> {
        match self {
            TableBackend::Memory(data) => {
                let mut rows: Vec<_> = data
                    .rows
                    .iter()
                    .map(|(&id, row)| (id, row.clone()))
                    .collect();
                rows.sort_by_key(|(id, _)| *id);
                Ok(rows)
            }
            TableBackend::Paged(pt) => pt.lock().scan(),
        }
    }

    fn update(&mut self, id: RowId, row: Row) -> crate::error::Result<bool> {
        match self {
            TableBackend::Memory(data) => Ok(data.rows.insert(id, row).is_some()),
            TableBackend::Paged(pt) => pt.lock().update(id, row),
        }
    }

    fn delete(&mut self, id: RowId) -> crate::error::Result<bool> {
        match self {
            TableBackend::Memory(data) => Ok(data.rows.remove(&id).is_some()),
            TableBackend::Paged(pt) => pt.lock().delete(id),
        }
    }

    fn row_count(&self) -> u64 {
        match self {
            TableBackend::Memory(data) => data.rows.len() as u64,
            TableBackend::Paged(pt) => pt.lock().row_count(),
        }
    }

    fn flush(&self) -> crate::error::Result<()> {
        match self {
            TableBackend::Memory(_) => Ok(()),
            TableBackend::Paged(pt) => pt.lock().flush_all(),
        }
    }

    fn cache_len(&self) -> usize {
        match self {
            TableBackend::Memory(_) => 0,
            TableBackend::Paged(pt) => pt.lock().cache_len(),
        }
    }
}

/// Storage tuning — `max_cached_pages = 0` keeps legacy in-memory `.dat` tables.
#[derive(Debug, Clone, Copy)]
pub struct StorageOptions {
    pub max_cached_pages: usize,
    pub rows_per_page: usize,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            max_cached_pages: 0,
            rows_per_page: DEFAULT_ROWS_PER_PAGE,
        }
    }
}

/// Row store with WAL-backed durability and periodic checkpointing.
pub struct StorageEngine {
    data_dir: PathBuf,
    options: StorageOptions,
    tables: RwLock<HashMap<String, TableBackend>>,
    wal: Arc<WriteAheadLog>,
}

impl StorageEngine {
    pub fn open(data_dir: impl AsRef<Path>, wal: Arc<WriteAheadLog>) -> crate::error::Result<Self> {
        Self::open_with_options(data_dir, wal, StorageOptions::default())
    }

    pub fn open_with_options(
        data_dir: impl AsRef<Path>,
        wal: Arc<WriteAheadLog>,
        options: StorageOptions,
    ) -> crate::error::Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir)?;

        let engine = Self {
            data_dir,
            options,
            tables: RwLock::new(HashMap::new()),
            wal,
        };
        engine.load_all()?;
        Ok(engine)
    }

    pub fn options(&self) -> StorageOptions {
        self.options
    }

    pub fn wal(&self) -> &Arc<WriteAheadLog> {
        &self.wal
    }

    pub fn page_cache_len(&self, table: &str) -> usize {
        self.tables
            .read()
            .get(table)
            .map(|t| t.cache_len())
            .unwrap_or(0)
    }

    fn table_path(&self, table: &str) -> PathBuf {
        self.data_dir.join(format!("{table}.dat"))
    }

    fn pages_path(&self, table: &str) -> PathBuf {
        self.data_dir.join(format!("{table}.pages"))
    }

    fn use_paged(&self) -> bool {
        self.options.max_cached_pages > 0
    }

    fn load_all(&self) -> crate::error::Result<()> {
        if !self.data_dir.exists() {
            return Ok(());
        }
        let mut names = std::collections::HashSet::new();
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if path.extension().is_some_and(|e| e == "dat" || e == "pages") {
                    names.insert(stem.to_string());
                }
            }
        }
        for name in names {
            let backend = self.load_table(&name)?;
            self.tables.write().insert(name, backend);
        }
        Ok(())
    }

    fn load_table(&self, table: &str) -> crate::error::Result<TableBackend> {
        let pages_path = self.pages_path(table);
        let dat_path = self.table_path(table);

        if pages_path.exists() {
            let pt = PageTable::open(
                &self.data_dir,
                table,
                self.options.max_cached_pages.max(1),
                self.options.rows_per_page,
            )?;
            return Ok(TableBackend::Paged(Mutex::new(pt)));
        }

        if self.use_paged() {
            let mut pt = PageTable::open(
                &self.data_dir,
                table,
                self.options.max_cached_pages,
                self.options.rows_per_page,
            )?;
            if dat_path.exists() {
                let legacy = Self::read_table_file(&dat_path)?;
                pt.import_rows(legacy.next_id, legacy.rows)?;
                pt.flush_all()?;
                fs::remove_file(&dat_path)?;
            }
            return Ok(TableBackend::Paged(Mutex::new(pt)));
        }

        if dat_path.exists() {
            return Ok(TableBackend::Memory(Self::read_table_file(&dat_path)?));
        }

        Ok(if self.use_paged() {
            TableBackend::Paged(Mutex::new(PageTable::open(
                &self.data_dir,
                table,
                self.options.max_cached_pages,
                self.options.rows_per_page,
            )?))
        } else {
            TableBackend::Memory(TableData::default())
        })
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
        let Some(backend) = tables.get(table) else {
            return Err(crate::error::MoodengError::TableNotFound(table.into()));
        };

        match backend {
            TableBackend::Memory(data) => {
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
            }
            TableBackend::Paged(pt) => {
                pt.lock().flush_all()?;
            }
        }
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
            .or_insert_with(|| {
                if self.use_paged() {
                    TableBackend::Paged(Mutex::new(
                        PageTable::open(
                            &self.data_dir,
                            table,
                            self.options.max_cached_pages,
                            self.options.rows_per_page,
                        )
                        .expect("open page table"),
                    ))
                } else {
                    TableBackend::Memory(TableData::default())
                }
            });
    }

    pub fn insert(&self, table: &str, row: Row, txn_id: u64) -> crate::error::Result<RowId> {
        let mut tables = self.tables.write();
        let backend = tables
            .entry(table.to_string())
            .or_insert_with(|| self.new_empty_backend(table));

        let id = backend.insert(row.clone())?;
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

    fn new_empty_backend(&self, table: &str) -> TableBackend {
        if self.use_paged() {
            TableBackend::Paged(Mutex::new(
                PageTable::open(
                    &self.data_dir,
                    table,
                    self.options.max_cached_pages,
                    self.options.rows_per_page,
                )
                .expect("open page table"),
            ))
        } else {
            TableBackend::Memory(TableData::default())
        }
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
        let backend = tables
            .entry(table.to_string())
            .or_insert_with(|| self.new_empty_backend(table));
        backend.apply_insert(row_id, row.clone())?;
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
        let Some(backend) = tables.get(table) else {
            return Ok(None);
        };
        backend.get(id)
    }

    pub fn scan(&self, table: &str) -> crate::error::Result<Vec<(RowId, Row)>> {
        let tables = self.tables.read();
        let Some(backend) = tables.get(table) else {
            return Ok(Vec::new());
        };
        backend.scan()
    }

    pub fn fetch_by_ids(
        &self,
        table: &str,
        ids: &[RowId],
    ) -> crate::error::Result<Vec<(RowId, Row)>> {
        let tables = self.tables.read();
        let Some(backend) = tables.get(table) else {
            return Err(crate::error::MoodengError::TableNotFound(table.into()));
        };

        let mut rows = Vec::new();
        for &id in ids {
            if let Some(row) = backend.get(id)? {
                rows.push((id, row));
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
        let backend = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let Some(existing) = backend.get(id)? else {
            return Ok(false);
        };

        if existing.version != expected_version {
            return Err(crate::error::MoodengError::VersionConflict {
                table: table.to_string(),
                row_id: id,
            });
        }

        row.version = expected_version + 1;
        backend.update(id, row.clone())?;
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
            let backend = tables
                .get_mut(table)
                .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

            let Some(existing) = backend.get(id)? else {
                return Ok(false);
            };

            let mut new_row = row.clone();
            new_row.version = existing.version + 1;
            backend.update(id, new_row.clone())?;
            if log { Some(new_row) } else { None }
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
        let backend = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        if backend.delete(id)? {
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

    pub fn apply_delete(
        &self,
        table: &str,
        id: RowId,
        log: bool,
        txn_id: u64,
    ) -> crate::error::Result<bool> {
        let mut tables = self.tables.write();
        let backend = tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        let deleted = backend.delete(id)?;
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
        let dat = self.table_path(table);
        if dat.exists() {
            fs::remove_file(dat)?;
        }
        let pages = self.pages_path(table);
        if pages.exists() {
            fs::remove_file(pages)?;
        }
        Ok(())
    }

    pub fn row_count(&self, table: &str) -> u64 {
        self.tables
            .read()
            .get(table)
            .map(|t| t.row_count())
            .unwrap_or(0)
    }
}
