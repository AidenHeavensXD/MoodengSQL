use crate::storage::StorageEngine;
use crate::wal::{WalOp, WriteAheadLog};

pub fn replay_wal(
    storage: &StorageEngine,
    wal: &WriteAheadLog,
) -> crate::error::Result<usize> {
    let from = wal.checkpoint_lsn();
    let entries = wal.replay_since(from)?;
    let count = entries.len();

    for (_, op) in entries {
        apply_wal_op(storage, op, false)?;
    }

    Ok(count)
}

pub fn apply_wal_op(
    storage: &StorageEngine,
    op: WalOp,
    log: bool,
) -> crate::error::Result<()> {
    match op {
        WalOp::Insert { table, row_id, row } => {
            storage.apply_insert(&table, row_id, row, log)?;
        }
        WalOp::Update { table, row_id, row } => {
            storage.apply_update(&table, row_id, row, log)?;
        }
        WalOp::Delete { table, row_id } => {
            storage.apply_delete(&table, row_id, log)?;
        }
    }
    Ok(())
}

pub fn checkpoint(storage: &StorageEngine, wal: &WriteAheadLog) -> crate::error::Result<()> {
    storage.checkpoint_all()?;
    let lsn = wal.current_lsn();
    wal.mark_checkpoint(lsn)?;
    wal.truncate()?;
    Ok(())
}
