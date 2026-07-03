use dashmap::DashMap;
use parking_lot::{RwLock, RwLockWriteGuard};
use std::sync::Arc;

use crate::storage::RowId;

/// Concurrency control: DDL uses table-level exclusive locks;
/// DML uses row-level locks so concurrent writes to different rows proceed in parallel.
#[derive(Debug)]
pub struct LockManager {
    ddl_locks: DashMap<String, Arc<RwLock<()>>>,
    row_locks: Arc<DashMap<(String, RowId), Arc<RwLock<()>>>>,
}

impl Default for LockManager {
    fn default() -> Self {
        Self {
            ddl_locks: DashMap::new(),
            row_locks: Arc::new(DashMap::new()),
        }
    }
}

/// Handle to a row lock entry; use [`RowLockHandle::write`] to acquire exclusivity.
pub struct RowLockHandle {
    row_locks: Arc<DashMap<(String, RowId), Arc<RwLock<()>>>>,
    key: (String, RowId),
    lock: Arc<RwLock<()>>,
}

/// Row write guard that evicts the DashMap entry when released if nothing else references it.
pub struct RowLockWriteGuard<'a> {
    handle: &'a RowLockHandle,
    inner: RwLockWriteGuard<'a, ()>,
}

impl Drop for RowLockWriteGuard<'_> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.handle.lock) == 2 {
            self.handle.row_locks.remove(&self.handle.key);
        }
    }
}

impl RowLockHandle {
    pub fn write(&self) -> RowLockWriteGuard<'_> {
        RowLockWriteGuard {
            handle: self,
            inner: self.lock.write(),
        }
    }
}

impl LockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn row_locks_len(&self) -> usize {
        self.row_locks.len()
    }

    /// Exclusive lock for schema changes (CREATE/DROP TABLE, CREATE INDEX).
    pub fn ddl_lock_for(&self, table: &str) -> Arc<RwLock<()>> {
        self.ddl_locks
            .entry(table.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// Row-level exclusive lock for UPDATE/DELETE on a specific row.
    pub fn row_lock(&self, table: &str, row_id: RowId) -> RowLockHandle {
        let key = (table.to_string(), row_id);
        let lock = self
            .row_locks
            .entry(key.clone())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone();
        RowLockHandle {
            row_locks: Arc::clone(&self.row_locks),
            key,
            lock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_locks_do_not_grow_unbounded_on_repeated_access() {
        let lm = LockManager::new();
        for _ in 0..100_000 {
            let handle = lm.row_lock("t", 1);
            let _guard = handle.write();
        }
        assert_eq!(lm.row_locks_len(), 0);
    }
}
