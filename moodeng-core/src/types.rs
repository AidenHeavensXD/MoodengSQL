use serde::{Deserialize, Serialize};
use std::fmt;

/// PostgreSQL-compatible data types optimized for speed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataType {
    Int4,
    Int8,
    Float4,
    Float8,
    Text,
    Varchar(usize),
    Bool,
    Timestamp,
    Json,
}

impl DataType {
    pub fn from_sql(name: &str) -> Option<Self> {
        let upper = name.to_uppercase();
        match upper.as_str() {
            "INT" | "INTEGER" | "INT4" => Some(DataType::Int4),
            "BIGINT" | "INT8" => Some(DataType::Int8),
            "REAL" | "FLOAT4" => Some(DataType::Float4),
            "DOUBLE PRECISION" | "FLOAT8" | "FLOAT" => Some(DataType::Float8),
            "TEXT" | "STRING" => Some(DataType::Text),
            "BOOLEAN" | "BOOL" => Some(DataType::Bool),
            "TIMESTAMP" | "TIMESTAMPTZ" => Some(DataType::Timestamp),
            "JSON" | "JSONB" => Some(DataType::Json),
            s if s.starts_with("VARCHAR") => {
                let len = s
                    .trim_start_matches("VARCHAR")
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .parse()
                    .unwrap_or(255);
                Some(DataType::Varchar(len))
            }
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            DataType::Int4 => "INT4",
            DataType::Int8 => "INT8",
            DataType::Float4 => "FLOAT4",
            DataType::Float8 => "FLOAT8",
            DataType::Text => "TEXT",
            DataType::Varchar(_) => "VARCHAR",
            DataType::Bool => "BOOL",
            DataType::Timestamp => "TIMESTAMP",
            DataType::Json => "JSON",
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Varchar(n) => write!(f, "VARCHAR({n})"),
            other => write!(f, "{}", other.name()),
        }
    }
}

/// Runtime value stored in rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Int4(i32),
    Int8(i64),
    Float4(f32),
    Float8(f64),
    Text(String),
    Bool(bool),
    Timestamp(i64),
    Json(serde_json::Value),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Int4(_) => "INT4",
            Value::Int8(_) => "INT8",
            Value::Float4(_) => "FLOAT4",
            Value::Float8(_) => "FLOAT8",
            Value::Text(_) => "TEXT",
            Value::Bool(_) => "BOOL",
            Value::Timestamp(_) => "TIMESTAMP",
            Value::Json(_) => "JSON",
        }
    }

    pub fn to_display_string(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Int4(v) => v.to_string(),
            Value::Int8(v) => v.to_string(),
            Value::Float4(v) => v.to_string(),
            Value::Float8(v) => v.to_string(),
            Value::Text(v) => v.clone(),
            Value::Bool(v) => v.to_string(),
            Value::Timestamp(v) => v.to_string(),
            Value::Json(v) => v.to_string(),
        }
    }

    pub fn from_sql_literal(lit: &sqlparser::ast::Value) -> Self {
        use sqlparser::ast::Value as SqlValue;
        match lit {
            SqlValue::Number(n, _) => {
                if n.contains('.') {
                    Value::Float8(n.parse().unwrap_or(0.0))
                } else {
                    Value::Int8(n.parse().unwrap_or(0))
                }
            }
            SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => {
                Value::Text(s.clone())
            }
            SqlValue::Boolean(b) => Value::Bool(*b),
            SqlValue::Null => Value::Null,
            _ => Value::Text(format!("{lit:?}")),
        }
    }

    pub fn coerce_to(&self, target: &DataType) -> crate::error::Result<Value> {
        if matches!(self, Value::Null) {
            return Ok(Value::Null);
        }
        match (self, target) {
            (Value::Int4(v), DataType::Int8) => Ok(Value::Int8(*v as i64)),
            (Value::Int8(v), DataType::Int4) => Ok(Value::Int4(*v as i32)),
            (Value::Int4(v), DataType::Float8) => Ok(Value::Float8(*v as f64)),
            (Value::Int8(v), DataType::Float8) => Ok(Value::Float8(*v as f64)),
            (Value::Float4(v), DataType::Float8) => Ok(Value::Float8(*v as f64)),
            (a, b) if a.type_name() == b.name() || matches!(b, DataType::Varchar(_)) => {
                Ok(a.clone())
            }
            (a, b) => Err(crate::error::MoodengError::TypeMismatch {
                expected: b.to_string(),
                actual: a.type_name().into(),
            }),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
    /// Optimistic concurrency version — incremented on every successful UPDATE.
    #[serde(default = "default_row_version")]
    pub version: u64,
}

fn default_row_version() -> u64 {
    1
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self {
            values,
            version: 1,
        }
    }
}

/// Query result returned to clients.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub rows_affected: u64,
    pub message: Option<String>,
    pub meta_changed: bool,
}

impl QueryResult {
    pub fn empty(message: impl Into<String>) -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            rows_affected: 0,
            message: Some(message.into()),
            meta_changed: false,
        }
    }

    pub fn select(columns: Vec<String>, rows: Vec<Row>) -> Self {
        let count = rows.len() as u64;
        Self {
            columns,
            rows,
            rows_affected: count,
            message: None,
            meta_changed: false,
        }
    }

    pub fn modified(rows_affected: u64, message: impl Into<String>) -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            rows_affected,
            message: Some(message.into()),
            meta_changed: false,
        }
    }

    pub fn ddl(message: impl Into<String>) -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            rows_affected: 0,
            message: Some(message.into()),
            meta_changed: true,
        }
    }
}
