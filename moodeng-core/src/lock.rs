use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;

use crate::storage::RowId;

/// Concurrency control: DDL uses table-level exclusive locks;
/// DML uses row-level locks so concurrent writes to different rows proceed in parallel.
#[derive(Debug, Default)]
pub struct LockManager {
    ddl_locks: DashMap<String, Arc<RwLock<()>>>,
    row_locks: DashMap<(String, RowId), Arc<RwLock<()>>>,
}

impl LockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exclusive lock for schema changes (CREATE/DROP TABLE, CREATE INDEX).
    pub fn ddl_lock_for(&self, table: &str) -> Arc<RwLock<()>> {
        self.ddl_locks
            .entry(table.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// Row-level exclusive lock for UPDATE/DELETE on a specific row.
    pub fn row_lock_for(&self, table: &str, row_id: RowId) -> Arc<RwLock<()>> {
        self.row_locks
            .entry((table.to_string(), row_id))
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }
}
