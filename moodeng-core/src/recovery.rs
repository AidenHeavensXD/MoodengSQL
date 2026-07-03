use std::collections::HashMap;

use crate::storage::StorageEngine;
use crate::wal::{WalOp, WriteAheadLog};

#[derive(Debug, Default)]
struct TxnState {
    committed: bool,
    aborted: bool,
    data_ops: Vec<(u64, WalOp)>,
}

pub fn replay_wal(
    storage: &StorageEngine,
    wal: &WriteAheadLog,
) -> crate::error::Result<usize> {
    let from = wal.checkpoint_lsn();
    let entries = wal.replay_since(from)?;

    // Pass 1: group ops by txn_id and determine commit/abort status.
    let mut txns: HashMap<u64, TxnState> = HashMap::new();

    for (lsn, op) in &entries {
        if let Some(txn_id) = op.control_txn_id() {
            let state = txns.entry(txn_id).or_default();
            match op {
                WalOp::Begin { .. } => {}
                WalOp::Commit { .. } => {
                    state.committed = true;
                    state.aborted = false;
                }
                WalOp::Abort { .. } => {
                    state.aborted = true;
                    state.committed = false;
                }
                _ => {}
            }
        } else if op.is_data_op() {
            if let Some(txn_id) = op.data_txn_id() {
                txns.entry(txn_id)
                    .or_default()
                    .data_ops
                    .push((*lsn, op.clone()));
            }
        }
    }

    // Pass 2: apply data ops only from committed, non-aborted transactions (LSN order).
    let mut to_apply = Vec::new();
    for state in txns.values() {
        if state.committed && !state.aborted {
            to_apply.extend(state.data_ops.iter().cloned());
        }
    }
    to_apply.sort_by_key(|(lsn, _)| *lsn);

    let count = to_apply.len();
    for (_, op) in to_apply {
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
        WalOp::Begin { .. } | WalOp::Commit { .. } | WalOp::Abort { .. } => Ok(()),
        WalOp::Insert {
            table, row_id, row, ..
        } => {
            storage.apply_insert(&table, row_id, row, log, 0)?;
            Ok(())
        }
        WalOp::Update {
            table, row_id, row, ..
        } => {
            storage.apply_update(&table, row_id, row, log, 0)?;
            Ok(())
        }
        WalOp::Delete {
            table, row_id, ..
        } => {
            storage.apply_delete(&table, row_id, log, 0)?;
            Ok(())
        }
    }
}

pub fn checkpoint(storage: &StorageEngine, wal: &WriteAheadLog) -> crate::error::Result<()> {
    storage.checkpoint_all()?;
    let lsn = wal.current_lsn();
    wal.mark_checkpoint(lsn)?;
    wal.truncate()?;
    Ok(())
}
