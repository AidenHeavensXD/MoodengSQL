use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::storage::RowId;
use crate::types::Row;

const CHECKPOINT_INTERVAL: u64 = 50;
const WAL_FILE: &str = "wal.log";
const CHECKPOINT_FILE: &str = "checkpoint.bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalOp {
    Insert {
        table: String,
        row_id: RowId,
        row: Row,
    },
    Update {
        table: String,
        row_id: RowId,
        row: Row,
    },
    Delete {
        table: String,
        row_id: RowId,
    },
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
    ops_since_checkpoint: Mutex<u64>,
    checkpoint_lsn: u64,
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
            ops_since_checkpoint: Mutex::new(0),
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
            let len = u32::from_be_bytes(len_buf) as u64;
            f.seek(SeekFrom::Current(len as i64))?;
            max_lsn = max_lsn.max(lsn);
        }
        Ok(max_lsn)
    }

    pub fn checkpoint_lsn(&self) -> u64 {
        self.checkpoint_lsn
    }

    pub fn append(&self, op: WalOp) -> crate::error::Result<u64> {
        let payload = bincode::serialize(&op)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;

        let mut lsn_guard = self.lsn.lock();
        *lsn_guard += 1;
        let lsn = *lsn_guard;
        drop(lsn_guard);

        let mut file = self.file.lock();
        file.write_all(&lsn.to_be_bytes())?;
        file.write_all(&(payload.len() as u32).to_be_bytes())?;
        file.write_all(&payload)?;
        file.sync_all()?;

        *self.ops_since_checkpoint.lock() += 1;
        Ok(lsn)
    }

    pub fn should_checkpoint(&self) -> bool {
        *self.ops_since_checkpoint.lock() >= CHECKPOINT_INTERVAL
    }

    pub fn mark_checkpoint(&self, lsn: u64) -> crate::error::Result<()> {
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
            let len = u32::from_be_bytes(len_buf) as usize;

            let mut payload = vec![0u8; len];
            f.read_exact(&mut payload)?;

            if lsn > from_lsn {
                let op: WalOp = bincode::deserialize(&payload)
                    .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
                entries.push((lsn, op));
            }
        }

        Ok(entries)
    }

    pub fn truncate(&self) -> crate::error::Result<()> {
        let mut file = OpenOptions::new()
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
