use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::storage::RowId;
use crate::types::Row;

const CHECKPOINT_INTERVAL: u64 = 50;
const WAL_SYNC_BATCH: u64 = 10;
const WAL_FILE: &str = "wal.log";
const CHECKPOINT_FILE: &str = "checkpoint.bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalOp {
    Begin {
        txn_id: u64,
    },
    Commit {
        txn_id: u64,
    },
    Abort {
        txn_id: u64,
    },
    Insert {
        txn_id: u64,
        table: String,
        row_id: RowId,
        row: Row,
    },
    Update {
        txn_id: u64,
        table: String,
        row_id: RowId,
        row: Row,
    },
    Delete {
        txn_id: u64,
        table: String,
        row_id: RowId,
    },
}

impl WalOp {
    pub fn data_txn_id(&self) -> Option<u64> {
        match self {
            WalOp::Insert { txn_id, .. }
            | WalOp::Update { txn_id, .. }
            | WalOp::Delete { txn_id, .. } => Some(*txn_id),
            WalOp::Begin { .. } | WalOp::Commit { .. } | WalOp::Abort { .. } => None,
        }
    }

    pub fn control_txn_id(&self) -> Option<u64> {
        match self {
            WalOp::Begin { txn_id }
            | WalOp::Commit { txn_id }
            | WalOp::Abort { txn_id } => Some(*txn_id),
            _ => None,
        }
    }

    pub fn is_data_op(&self) -> bool {
        matches!(
            self,
            WalOp::Insert { .. } | WalOp::Update { .. } | WalOp::Delete { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointState {
    lsn: u64,
}

#[derive(Debug)]
pub struct WriteAheadLog {
    wal_path: PathBuf,
    checkpoint_path: PathBuf,
    file: Mutex<File>,
    lsn: Mutex<u64>,
    next_txn_id: Mutex<u64>,
    ops_since_checkpoint: Mutex<u64>,
    unsynced_writes: Mutex<u64>,
    checkpoint_lsn: u64,
}

fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0u32;
        while i < 256 {
            let mut c = i;
            let mut k = 0;
            while k < 8 {
                if c & 1 != 0 {
                    c = 0xEDB8_8320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
                k += 1;
            }
            table[i as usize] = c;
            i += 1;
        }
        table
    };

    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = TABLE[idx] ^ (crc >> 8);
    }
    !crc
}

impl WriteAheadLog {
    pub fn open(data_dir: impl AsRef<Path>) -> crate::error::Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir)?;

        let wal_path = data_dir.join(WAL_FILE);
        let checkpoint_path = data_dir.join(CHECKPOINT_FILE);

        let checkpoint_lsn = if checkpoint_path.exists() {
            let mut f = File::open(&checkpoint_path)?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            if buf.is_empty() {
                0
            } else {
                bincode::deserialize::<CheckpointState>(&buf)
                    .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?
                    .lsn
            }
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&wal_path)?;

        let current_lsn = Self::read_max_lsn(&wal_path)?;

        Ok(Self {
            wal_path,
            checkpoint_path,
            file: Mutex::new(file),
            lsn: Mutex::new(current_lsn),
            next_txn_id: Mutex::new(1),
            ops_since_checkpoint: Mutex::new(0),
            unsynced_writes: Mutex::new(0),
            checkpoint_lsn,
        })
    }

    fn read_max_lsn(wal_path: &Path) -> crate::error::Result<u64> {
        if !wal_path.exists() {
            return Ok(0);
        }
        let mut f = File::open(wal_path)?;
        let mut max_lsn = 0u64;
        loop {
            let mut lsn_buf = [0u8; 8];
            if f.read_exact(&mut lsn_buf).is_err() {
                break;
            }
            let lsn = u64::from_be_bytes(lsn_buf);

            let mut len_buf = [0u8; 4];
            if f.read_exact(&mut len_buf).is_err() {
                break;
            }
            let body_len = u32::from_be_bytes(len_buf) as u64;
            if body_len < 4 {
                break;
            }
            if f.seek(SeekFrom::Current(body_len as i64)).is_err() {
                break;
            }
            max_lsn = max_lsn.max(lsn);
        }
        Ok(max_lsn)
    }

    pub fn checkpoint_lsn(&self) -> u64 {
        self.checkpoint_lsn
    }

    pub fn alloc_txn_id(&self) -> u64 {
        let mut next = self.next_txn_id.lock();
        let id = *next;
        *next += 1;
        id
    }

    pub fn append(&self, op: WalOp) -> crate::error::Result<u64> {
        let payload = bincode::serialize(&op)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
        let checksum = crc32(&payload);

        let mut lsn_guard = self.lsn.lock();
        *lsn_guard += 1;
        let lsn = *lsn_guard;
        drop(lsn_guard);

        let body_len = 4 + payload.len();
        let mut file = self.file.lock();
        file.write_all(&lsn.to_be_bytes())?;
        file.write_all(&(body_len as u32).to_be_bytes())?;
        file.write_all(&checksum.to_be_bytes())?;
        file.write_all(&payload)?;

        *self.ops_since_checkpoint.lock() += 1;
        let mut unsynced = self.unsynced_writes.lock();
        *unsynced += 1;
        if *unsynced >= WAL_SYNC_BATCH {
            file.sync_all()?;
            *unsynced = 0;
        }

        Ok(lsn)
    }

    /// Force WAL data to durable storage (used before checkpoint/truncate).
    pub fn flush(&self) -> crate::error::Result<()> {
        let mut file = self.file.lock();
        file.sync_all()?;
        *self.unsynced_writes.lock() = 0;
        Ok(())
    }

    pub fn should_checkpoint(&self) -> bool {
        *self.ops_since_checkpoint.lock() >= CHECKPOINT_INTERVAL
    }

    pub fn mark_checkpoint(&self, lsn: u64) -> crate::error::Result<()> {
        self.flush()?;
        let state = CheckpointState { lsn };
        let encoded = bincode::serialize(&state)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.checkpoint_path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;

        *self.ops_since_checkpoint.lock() = 0;
        Ok(())
    }

    /// Read WAL entries after `from_lsn`, stopping at the first torn/invalid entry.
    pub fn replay_since(&self, from_lsn: u64) -> crate::error::Result<Vec<(u64, WalOp)>> {
        if !self.wal_path.exists() {
            return Ok(Vec::new());
        }

        let mut f = File::open(&self.wal_path)?;
        let mut entries = Vec::new();

        loop {
            let mut lsn_buf = [0u8; 8];
            if f.read_exact(&mut lsn_buf).is_err() {
                break;
            }
            let lsn = u64::from_be_bytes(lsn_buf);

            let mut len_buf = [0u8; 4];
            if f.read_exact(&mut len_buf).is_err() {
                break;
            }
            let body_len = u32::from_be_bytes(len_buf) as usize;
            if body_len < 4 {
                break;
            }

            let mut crc_buf = [0u8; 4];
            if f.read_exact(&mut crc_buf).is_err() {
                break;
            }
            let stored_crc = u32::from_be_bytes(crc_buf);

            let payload_len = body_len - 4;
            let mut payload = vec![0u8; payload_len];
            if f.read_exact(&mut payload).is_err() {
                break;
            }

            if crc32(&payload) != stored_crc {
                break;
            }

            let op: WalOp = match bincode::deserialize(&payload) {
                Ok(op) => op,
                Err(_) => break,
            };

            if lsn > from_lsn {
                entries.push((lsn, op));
            }
        }

        Ok(entries)
    }

    pub fn truncate(&self) -> crate::error::Result<()> {
        self.flush()?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.wal_path)?;
        file.sync_all()?;
        *self.lsn.lock() = 0;
        Ok(())
    }

    pub fn current_lsn(&self) -> u64 {
        *self.lsn.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_detects_corruption() {
        let payload = b"test payload";
        assert_eq!(crc32(payload), crc32(payload));
        assert_ne!(crc32(payload), crc32(b"test payloaX"));
    }
}
