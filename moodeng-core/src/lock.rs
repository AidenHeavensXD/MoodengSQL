use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct LockManager {
    table_locks: DashMap<String, Arc<RwLock<()>>>,
}

impl LockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock_for(&self, table: &str) -> Arc<RwLock<()>> {
        self.table_locks
            .entry(table.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }
}
