use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::catalog::Catalog;
use crate::executor::Executor;
use crate::index::IndexManager;
use crate::lock::LockManager;
use crate::meta::{reconcile_storage, DatabaseSnapshot, MetaStore};
use crate::recovery::replay_wal;
use crate::storage::StorageEngine;
use crate::transaction::Session;
use crate::types::QueryResult;
use crate::wal::WriteAheadLog;
use crate::{ENGINE_NAME, ENGINE_VERSION, OWNER};

/// Main MoodengSQL database instance.
pub struct Database {
    pub catalog: Arc<Catalog>,
    pub storage: Arc<StorageEngine>,
    pub indexes: Arc<IndexManager>,
    pub locks: Arc<LockManager>,
    wal: Arc<WriteAheadLog>,
    data_dir: PathBuf,
    stats: RwLock<DatabaseStats>,
}

#[derive(Debug, Clone, Default)]
pub struct DatabaseStats {
    pub queries_executed: u64,
    pub total_rows: u64,
}

impl Database {
    pub fn open(data_dir: impl AsRef<Path>) -> crate::error::Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;

        let wal = Arc::new(WriteAheadLog::open(&data_dir)?);
        let catalog = Arc::new(Catalog::new());
        let storage = Arc::new(StorageEngine::open(&data_dir, Arc::clone(&wal))?);
        let indexes = Arc::new(IndexManager::new());
        let locks = Arc::new(LockManager::new());
        let meta_store = MetaStore::new(&data_dir);

        if let Some(snapshot) = meta_store.load()? {
            catalog.load_tables(snapshot.tables);
            indexes.load_all(snapshot.indexes);
        }

        for table in catalog.list_tables() {
            storage.ensure_table(&table);
        }

        let replayed = replay_wal(&storage, &wal)?;
        if replayed > 0 {
            tracing::info!("replayed {replayed} WAL entries");
        }

        indexes.rebuild_all(&catalog, &storage)?;

        let db = Self {
            catalog,
            storage,
            indexes,
            locks,
            wal,
            data_dir,
            stats: RwLock::new(DatabaseStats::default()),
        };

        let warnings = reconcile_storage(&db.data_dir, &db.catalog.list_tables())?;
        for warning in warnings {
            tracing::warn!("{warning}");
        }

        Ok(db)
    }

    pub fn in_memory() -> crate::error::Result<Self> {
        Self::open(std::env::temp_dir().join(format!("moodengsql_mem_{}", uuid::Uuid::new_v4())))
    }

    pub fn execute(&self, sql: &str) -> crate::error::Result<QueryResult> {
        self.execute_session(&mut Session::new(), sql)
    }

    pub fn execute_session(
        &self,
        session: &mut Session,
        sql: &str,
    ) -> crate::error::Result<QueryResult> {
        let mut executor = Executor::new(
            &self.catalog,
            &self.storage,
            &self.indexes,
            &self.locks,
            session,
        );
        let result = executor.execute(sql)?;

        if result.meta_changed {
            self.persist()?;
        }

        let mut stats = self.stats.write();
        stats.queries_executed += 1;
        stats.total_rows = self
            .catalog
            .list_tables()
            .iter()
            .map(|t| self.storage.row_count(t))
            .sum();
        Ok(result)
    }

    pub fn persist(&self) -> crate::error::Result<()> {
        let snapshot = DatabaseSnapshot {
            version: 1,
            tables: self.catalog.snapshot(),
            indexes: self.indexes.snapshot(),
        };
        MetaStore::new(&self.data_dir).save(&snapshot)
    }

    pub fn check(&self) -> crate::error::Result<Vec<String>> {
        let mut issues = reconcile_storage(&self.data_dir, &self.catalog.list_tables())?;

        if issues.is_empty() {
            issues.push("ok: catalog and storage are consistent".into());
        }

        Ok(issues)
    }

    pub fn info(&self) -> String {
        format!(
            "{ENGINE_NAME} v{ENGINE_VERSION} — Owner: {OWNER}\nData directory: {}\nTables: {}",
            self.data_dir.display(),
            self.catalog.list_tables().len()
        )
    }

    pub fn stats(&self) -> DatabaseStats {
        self.stats.read().clone()
    }

    pub fn wal(&self) -> &Arc<WriteAheadLog> {
        &self.wal
    }

    /// Flush WAL to disk — useful for crash-recovery tests and durability guarantees.
    pub fn flush_wal(&self) -> crate::error::Result<()> {
        self.wal.flush()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}
