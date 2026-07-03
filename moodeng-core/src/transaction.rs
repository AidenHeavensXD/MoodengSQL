use crate::storage::{RowId, StorageEngine};
use crate::types::Row;

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
    undo: Vec<UndoRecord>,
}

impl Transaction {
    pub fn begin(&mut self) {
        self.active = true;
        self.undo.clear();
    }

    pub fn commit(&mut self) {
        self.active = false;
        self.undo.clear();
    }

    pub fn rollback(&mut self) -> Vec<UndoRecord> {
        self.active = false;
        self.undo.drain(..).rev().collect()
    }

    pub fn record(&mut self, entry: UndoRecord) {
        if self.active {
            self.undo.push(entry);
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

#[derive(Debug, Default)]
pub struct Session {
    pub transaction: Transaction,
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
                storage.apply_delete(table, *row_id, false)?;
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
            storage.apply_update(table, *row_id, old_row.clone(), false)?;
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
            storage.apply_insert(table, *row_id, old_row.clone(), false)?;
            indexes.insert_row(table, &old_row.values, &col_names, *row_id)?;
        }
    }
    Ok(())
}
