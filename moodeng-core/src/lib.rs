//! MoodengSQL Core Engine
//!
//! A high-performance, PostgreSQL-inspired relational database engine.
//!
//! **Owner:** AidenHeavensXD

pub mod backup;
pub mod catalog;
pub mod engine;
pub mod error;
pub mod executor;
pub mod index;
pub mod lock;
pub mod meta;
pub mod parser;
pub mod planner;
pub mod query_util;
pub mod recovery;
pub mod storage;
pub mod transaction;
pub mod types;
pub mod wal;

pub use backup::{backup, backup_live, list_backup_files, restore};
pub use engine::Database;
pub use error::{MoodengError, Result};
pub use transaction::Session;
pub use query_util::substitute_params;
pub use types::{ColumnDef, DataType, QueryResult, Row, Value};

pub const ENGINE_NAME: &str = "MoodengSQL";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const OWNER: &str = "AidenHeavensXD";
