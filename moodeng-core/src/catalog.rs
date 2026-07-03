use crate::types::{ColumnDef, DataType};
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Default)]
pub struct Catalog {
    tables: DashMap<String, TableSchema>,
    stats: RwLock<CatalogStats>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CatalogStats {
    pub table_count: usize,
    pub total_indexes: usize,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_table(&self, name: String, columns: Vec<ColumnDef>) -> crate::error::Result<()> {
        if self.tables.contains_key(&name) {
            return Err(crate::error::MoodengError::Execution(format!(
                "table '{name}' already exists"
            )));
        }
        self.tables.insert(
            name.clone(),
            TableSchema {
                name,
                columns,
                indexes: vec![],
            },
        );
        self.stats.write().table_count = self.tables.len();
        Ok(())
    }

    pub fn drop_table(&self, name: &str) -> crate::error::Result<()> {
        self.tables
            .remove(name)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(name.into()))?;
        self.stats.write().table_count = self.tables.len();
        Ok(())
    }

    pub fn get_table(&self, name: &str) -> crate::error::Result<TableSchema> {
        self.tables
            .get(name)
            .map(|t| t.clone())
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(name.into()))
    }

    pub fn list_tables(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tables.iter().map(|e| e.key().clone()).collect();
        names.sort();
        names
    }

    pub fn load_tables(&self, tables: Vec<TableSchema>) {
        for table in tables {
            self.tables.insert(table.name.clone(), table);
        }
        self.stats.write().table_count = self.tables.len();
    }

    pub fn snapshot(&self) -> Vec<TableSchema> {
        let mut tables: Vec<_> = self.tables.iter().map(|e| e.value().clone()).collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        tables
    }

    pub fn add_index(
        &self,
        table: &str,
        index: IndexMeta,
    ) -> crate::error::Result<()> {
        let mut entry = self
            .tables
            .get_mut(table)
            .ok_or_else(|| crate::error::MoodengError::TableNotFound(table.into()))?;

        if entry.indexes.iter().any(|i| i.name == index.name) {
            return Err(crate::error::MoodengError::IndexExists(index.name));
        }

        entry.indexes.push(index);
        self.stats.write().total_indexes += 1;
        Ok(())
    }

    pub fn column_index(&self, schema: &TableSchema, col: &str) -> Option<usize> {
        schema.columns.iter().position(|c| c.name.eq_ignore_ascii_case(col))
    }

    pub fn resolve_column_types(&self, schema: &TableSchema) -> HashMap<String, DataType> {
        schema
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone()))
            .collect()
    }
}
