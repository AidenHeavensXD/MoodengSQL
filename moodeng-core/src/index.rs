use crate::storage::{RowId, StorageEngine};
use crate::types::Value;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// B-tree index for fast point and range lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BTreeIndex {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    tree: BTreeMap<IndexKey, Vec<RowId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct IndexKey(Vec<ComparableValue>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum ComparableValue {
    Null,
    Int(i64),
    Float(u64),
    Text(String),
    Bool(bool),
}

impl From<&Value> for ComparableValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::Null => ComparableValue::Null,
            Value::Int4(n) => ComparableValue::Int(*n as i64),
            Value::Int8(n) => ComparableValue::Int(*n),
            Value::Float4(n) => ComparableValue::Float(n.to_bits() as u64),
            Value::Float8(n) => ComparableValue::Float(n.to_bits()),
            Value::Text(s) => ComparableValue::Text(s.clone()),
            Value::Bool(b) => ComparableValue::Bool(*b),
            Value::Timestamp(n) => ComparableValue::Int(*n),
            Value::Json(j) => ComparableValue::Text(j.to_string()),
        }
    }
}

impl BTreeIndex {
    pub fn new(name: String, columns: Vec<String>, unique: bool) -> Self {
        Self {
            name,
            columns,
            unique,
            tree: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key_values: &[Value], row_id: RowId) -> crate::error::Result<()> {
        let key = IndexKey(key_values.iter().map(ComparableValue::from).collect());

        if self.unique {
            if self.tree.contains_key(&key) {
                return Err(crate::error::MoodengError::DuplicateKey(format!(
                    "unique index '{}' violation",
                    self.name
                )));
            }
            self.tree.insert(key, vec![row_id]);
        } else {
            self.tree.entry(key).or_default().push(row_id);
        }
        Ok(())
    }

    pub fn remove(&mut self, key_values: &[Value], row_id: RowId) {
        let key = IndexKey(key_values.iter().map(ComparableValue::from).collect());
        if let Some(ids) = self.tree.get_mut(&key) {
            ids.retain(|&id| id != row_id);
            if ids.is_empty() {
                self.tree.remove(&key);
            }
        }
    }

    pub fn lookup(&self, key_values: &[Value]) -> Vec<RowId> {
        let key = IndexKey(key_values.iter().map(ComparableValue::from).collect());
        self.tree.get(&key).cloned().unwrap_or_default()
    }

    pub fn range_scan(&self, min: Option<&[Value]>, max: Option<&[Value]>) -> Vec<RowId> {
        let min_key = min.map(|v| IndexKey(v.iter().map(ComparableValue::from).collect()));
        let max_key = max.map(|v| IndexKey(v.iter().map(ComparableValue::from).collect()));

        let mut result = Vec::new();
        let iter: Box<dyn Iterator<Item = (&IndexKey, &Vec<RowId>)>> = match (&min_key, &max_key) {
            (Some(min), Some(max)) => Box::new(self.tree.range(min..=max)),
            (Some(min), None) => Box::new(self.tree.range(min..)),
            (None, Some(max)) => Box::new(self.tree.range(..=max)),
            (None, None) => Box::new(self.tree.iter()),
        };

        for (_, ids) in iter {
            result.extend(ids);
        }
        result
    }
}

/// Index manager per table.
#[derive(Debug, Default)]
pub struct IndexManager {
    indexes: RwLock<HashMap<String, HashMap<String, BTreeIndex>>>,
}

impl IndexManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_index(
        &self,
        table: &str,
        index: BTreeIndex,
    ) -> crate::error::Result<()> {
        let mut indexes = self.indexes.write();
        let table_indexes = indexes.entry(table.to_string()).or_default();

        if table_indexes.contains_key(&index.name) {
            return Err(crate::error::MoodengError::IndexExists(index.name));
        }

        table_indexes.insert(index.name.clone(), index);
        Ok(())
    }

    pub fn get_index(&self, table: &str, name: &str) -> Option<BTreeIndex> {
        self.indexes
            .read()
            .get(table)
            .and_then(|t| t.get(name).cloned())
    }

    pub fn table_indexes(&self, table: &str) -> Vec<BTreeIndex> {
        self.indexes
            .read()
            .get(table)
            .map(|t| t.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn insert_row(
        &self,
        table: &str,
        row_values: &[Value],
        column_names: &[String],
        row_id: RowId,
    ) -> crate::error::Result<()> {
        let mut indexes = self.indexes.write();
        if let Some(table_indexes) = indexes.get_mut(table) {
            for index in table_indexes.values_mut() {
                let key_vals: Vec<Value> = index
                    .columns
                    .iter()
                    .map(|col| {
                        let idx = column_names
                            .iter()
                            .position(|c| c.eq_ignore_ascii_case(col))
                            .unwrap_or(0);
                        row_values.get(idx).cloned().unwrap_or(Value::Null)
                    })
                    .collect();
                index.insert(&key_vals, row_id)?;
            }
        }
        Ok(())
    }

    pub fn remove_row(
        &self,
        table: &str,
        row_values: &[Value],
        column_names: &[String],
        row_id: RowId,
    ) {
        let mut indexes = self.indexes.write();
        if let Some(table_indexes) = indexes.get_mut(table) {
            for index in table_indexes.values_mut() {
                let key_vals: Vec<Value> = index
                    .columns
                    .iter()
                    .map(|col| {
                        let idx = column_names
                            .iter()
                            .position(|c| c.eq_ignore_ascii_case(col))
                            .unwrap_or(0);
                        row_values.get(idx).cloned().unwrap_or(Value::Null)
                    })
                    .collect();
                index.remove(&key_vals, row_id);
            }
        }
    }

    pub fn lookup(&self, table: &str, index_name: &str, key_values: &[Value]) -> Vec<RowId> {
        self.indexes
            .read()
            .get(table)
            .and_then(|t| t.get(index_name))
            .map(|idx| idx.lookup(key_values))
            .unwrap_or_default()
    }

    pub fn find_index_for_column(&self, table: &str, column: &str) -> Option<String> {
        self.indexes.read().get(table).and_then(|table_indexes| {
            table_indexes
                .iter()
                .find(|(_, idx)| {
                    idx.columns.len() == 1
                        && idx.columns[0].eq_ignore_ascii_case(column)
                })
                .map(|(name, _)| name.clone())
        })
    }

    pub fn drop_table(&self, table: &str) {
        self.indexes.write().remove(table);
    }

    pub fn load_all(&self, data: HashMap<String, HashMap<String, BTreeIndex>>) {
        *self.indexes.write() = data;
    }

    pub fn snapshot(&self) -> HashMap<String, HashMap<String, BTreeIndex>> {
        self.indexes.read().clone()
    }

    pub fn rebuild_all(
        &self,
        catalog: &crate::catalog::Catalog,
        storage: &StorageEngine,
    ) -> crate::error::Result<()> {
        let tables = catalog.list_tables();
        self.indexes.write().clear();

        for table in tables {
            let schema = catalog.get_table(&table)?;
            let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();

            for meta in &schema.indexes {
                let mut index =
                    BTreeIndex::new(meta.name.clone(), meta.columns.clone(), meta.unique);
                for (id, row) in storage.scan(&table)? {
                    let key_vals: Vec<Value> = meta
                        .columns
                        .iter()
                        .map(|col| {
                            let i = col_names
                                .iter()
                                .position(|c| c.eq_ignore_ascii_case(col))
                                .unwrap_or(0);
                            row.values.get(i).cloned().unwrap_or(Value::Null)
                        })
                        .collect();
                    index.insert(&key_vals, id)?;
                }
                self.create_index(&table, index)?;
            }
        }

        Ok(())
    }
}
