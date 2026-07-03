//! Property-based tests for WAL replay robustness.

use moodeng_core::{
    encode_wal_entry, replay_from_bytes, replay_wal, types::Row, types::Value, WalOp,
    WriteAheadLog,
};
use moodeng_core::storage::StorageEngine;
use proptest::prelude::*;
use std::sync::Arc;

fn arb_wal_op() -> impl Strategy<Value = WalOp> {
    prop_oneof![
        (1u64..100u64).prop_map(|txn_id| WalOp::Begin { txn_id }),
        (1u64..100u64).prop_map(|txn_id| WalOp::Commit { txn_id }),
        (1u64..100u64).prop_map(|txn_id| WalOp::Abort { txn_id }),
        (1u64..100u64, 1u64..1000u64, 0i32..1000).prop_map(|(txn_id, row_id, v)| {
            WalOp::Insert {
                txn_id,
                table: "t".into(),
                row_id,
                row: Row::new(vec![Value::Int4(v)]),
            }
        }),
    ]
}

proptest! {
    #[test]
    fn replay_random_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..8192)) {
        let _entries = replay_from_bytes(&data, 0);
    }

    #[test]
    fn replay_valid_stream_never_panics(
        ops in prop::collection::vec(arb_wal_op(), 0..32)
    ) {
        let mut bytes = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            bytes.extend(encode_wal_entry((i as u64) + 1, op));
        }
        let _entries = replay_from_bytes(&bytes, 0);
    }

    #[test]
    fn bad_checksum_stops_replay_early(
        ops in prop::collection::vec(arb_wal_op(), 2..8)
    ) {
        let mut bytes = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            bytes.extend(encode_wal_entry((i as u64) + 1, op));
        }

        let valid_count = replay_from_bytes(&bytes, 0).len();
        if !bytes.is_empty() {
            let last = bytes.len() - 1;
            bytes[last] ^= 0xFF;
        }

        let after_corrupt = replay_from_bytes(&bytes, 0);
        prop_assert!(after_corrupt.len() <= valid_count);
    }
}

#[test]
fn replay_wal_committed_only_applies_data_ops() {
    let dir = std::env::temp_dir().join(format!("wal_replay_prop_{}", uuid::Uuid::new_v4()));
    let wal = Arc::new(WriteAheadLog::open(&dir).unwrap());
    let storage = Arc::new(StorageEngine::open(&dir, Arc::clone(&wal)).unwrap());
    storage.ensure_table("t");

    let txn_ok = wal.alloc_txn_id();
    wal.append(WalOp::Begin { txn_id: txn_ok }).unwrap();
    wal.append(WalOp::Insert {
        txn_id: txn_ok,
        table: "t".into(),
        row_id: 1,
        row: Row::new(vec![Value::Int4(1)]),
    })
    .unwrap();
    wal.append(WalOp::Commit { txn_id: txn_ok }).unwrap();

    let txn_bad = wal.alloc_txn_id();
    wal.append(WalOp::Begin { txn_id: txn_bad }).unwrap();
    wal.append(WalOp::Insert {
        txn_id: txn_bad,
        table: "t".into(),
        row_id: 2,
        row: Row::new(vec![Value::Int4(2)]),
    })
    .unwrap();
    // no commit — simulates crash

    let applied = replay_wal(&storage, &wal).unwrap();
    assert_eq!(applied, 1);
    assert_eq!(storage.row_count("t"), 1);

    let _ = std::fs::remove_dir_all(&dir);
}
