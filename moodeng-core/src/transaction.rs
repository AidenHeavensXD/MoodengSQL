use crate::storage::{RowId, StorageEngine};
use crate::types::Row;
use crate::wal::{WalOp, WriteAheadLog};

#[derive(Debug, Clone)]
pub enum UndoRecord {
    Insert { table: String, row_id: RowId },
    Update {
        table: String,
        row_id: RowId,
        old_row: Row,
    },
    Delete {
        table: String,
        row_id: RowId,
        old_row: Row,
    },
}

#[derive(Debug, Default)]
pub struct Transaction {
    pub active: bool,
    pub txn_id: Option<u64>,
    undo: Vec<UndoRecord>,
}

impl Transaction {
    pub fn begin(&mut self, wal: &WriteAheadLog) -> crate::error::Result<()> {
        if self.active {
            return Err(crate::error::MoodengError::Execution(
                "transaction already active".into(),
            ));
        }
        let txn_id = wal.alloc_txn_id();
        wal.append(WalOp::Begin { txn_id })?;
        self.txn_id = Some(txn_id);
        self.active = true;
        self.undo.clear();
        Ok(())
    }

    pub fn commit(&mut self, wal: &WriteAheadLog) -> crate::error::Result<()> {
        let txn_id = self
            .txn_id
            .take()
            .ok_or_else(|| crate::error::MoodengError::Execution("no active transaction".into()))?;
        wal.append(WalOp::Commit { txn_id })?;
        self.active = false;
        self.undo.clear();
        Ok(())
    }

    pub fn rollback(&mut self, wal: &WriteAheadLog) -> crate::error::Result<Vec<UndoRecord>> {
        let txn_id = self
            .txn_id
            .take()
            .ok_or_else(|| crate::error::MoodengError::Execution("no active transaction".into()))?;
        wal.append(WalOp::Abort { txn_id })?;
        self.active = false;
        Ok(self.undo.drain(..).rev().collect())
    }

    pub fn record(&mut self, entry: UndoRecord) {
        if self.active {
            self.undo.push(entry);
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn current_txn_id(&self) -> Option<u64> {
        self.txn_id
    }
}

#[derive(Debug, Default)]
pub struct Session {
    pub transaction: Transaction,
    /// True when an implicit auto-commit transaction was opened for a single statement.
    pub auto_commit: bool,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn apply_undo(
    storage: &StorageEngine,
    indexes: &crate::index::IndexManager,
    catalog: &crate::catalog::Catalog,
    record: &UndoRecord,
) -> crate::error::Result<()> {
    match record {
        UndoRecord::Insert { table, row_id } => {
            if let Some(row) = storage.get(table, *row_id)? {
                let col_names: Vec<String> = catalog
                    .get_table(table)?
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                indexes.remove_row(table, &row.values, &col_names, *row_id);
                storage.apply_delete(table, *row_id, false, 0)?;
            }
        }
        UndoRecord::Update {
            table,
            row_id,
            old_row,
        } => {
            let col_names: Vec<String> = catalog
                .get_table(table)?
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect();
            if let Some(current) = storage.get(table, *row_id)? {
                indexes.remove_row(table, &current.values, &col_names, *row_id);
            }
            storage.apply_update(table, *row_id, old_row.clone(), false, 0)?;
            indexes.insert_row(table, &old_row.values, &col_names, *row_id)?;
        }
        UndoRecord::Delete {
            table,
            row_id,
            old_row,
        } => {
            let col_names: Vec<String> = catalog
                .get_table(table)?
                .columns
                .iter()
                .map(|c| c.name.clone())
                .collect();
            storage.apply_insert(table, *row_id, old_row.clone(), false, 0)?;
            indexes.insert_row(table, &old_row.values, &col_names, *row_id)?;
        }
    }
    Ok(())
}
