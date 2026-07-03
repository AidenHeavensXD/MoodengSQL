use sqlparser::ast::{
    Assignment, Expr, GroupByExpr, Join, JoinConstraint, JoinOperator, ObjectName, Query,
    SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
};

use parking_lot::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::catalog::{Catalog, IndexMeta};
use crate::index::{BTreeIndex, IndexManager};
use crate::lock::LockManager;
use crate::parser::{
    assignment_value, eval_expr, eval_where, extract_ident, extract_pk_from_constraints,
    extract_table_name, parse_sql, select_items_to_columns, sql_column_to_def,
    values_from_insert,
};
use crate::planner::{plan_scan, ScanPlan};
use crate::query_util::{apply_group_by, apply_limit_offset, sort_rows};
use crate::storage::StorageEngine;
use crate::transaction::{apply_undo, Session, UndoRecord};
use crate::types::{ColumnDef, QueryResult, Row, Value};

pub struct Executor<'a> {
    catalog: &'a Catalog,
    storage: &'a StorageEngine,
    indexes: &'a IndexManager,
    locks: &'a LockManager,
    backup_lock: &'a Arc<RwLock<()>>,
    active_write_txns: &'a Arc<AtomicUsize>,
    session: &'a mut Session,
}

impl<'a> Executor<'a> {
    pub fn new(
        catalog: &'a Catalog,
        storage: &'a StorageEngine,
        indexes: &'a IndexManager,
        locks: &'a LockManager,
        backup_lock: &'a Arc<RwLock<()>>,
        active_write_txns: &'a Arc<AtomicUsize>,
        session: &'a mut Session,
    ) -> Self {
        Self {
            catalog,
            storage,
            indexes,
            locks,
            backup_lock,
            active_write_txns,
            session,
        }
    }

    pub fn execute(&mut self, sql: &str) -> crate::error::Result<QueryResult> {
        let statements = parse_sql(sql)?;
        if statements.is_empty() {
            return Ok(QueryResult::empty("no statements"));
        }
        let mut last = QueryResult::empty("ok");
        for stmt in &statements {
            last = self.execute_statement(stmt)?;
        }
        Ok(last)
    }

    fn execute_statement(&mut self, stmt: &Statement) -> crate::error::Result<QueryResult> {
        match stmt {
            Statement::StartTransaction { .. } => self.begin(),
            Statement::Commit { .. } => self.commit(),
            Statement::Rollback { .. } => self.rollback(),
            Statement::CreateTable(create) => {
                self.create_table(&create.name, &create.columns, &create.constraints)
            }
            Statement::Drop { object_type, names, .. } => {
                if object_type.to_string().contains("TABLE") {
                    self.drop_table(&extract_table_name(&names[0]))
                } else {
                    Err(crate::error::MoodengError::Execution(format!(
                        "unsupported DROP: {object_type}"
                    )))
                }
            }
            Statement::Insert(insert) => {
                let table = extract_table_name(&insert.table_name);
                let source = insert.source.as_ref().ok_or_else(|| {
                    crate::error::MoodengError::Parse("missing INSERT source".into())
                })?;
                self.insert(&table, source)
            }
            Statement::Query(query) => self.select(query),
            Statement::Update { table, assignments, selection, .. } => {
                let table_name = table_factor_name(&table.relation);
                self.update(&table_name, assignments, selection.as_ref())
            }
            Statement::Delete(delete) => {
                let table_name = extract_delete_table(&delete.from);
                self.delete(&table_name, delete.selection.as_ref())
            }
            Statement::CreateIndex(create) => {
                let table = extract_table_name(&create.table_name);
                let index_name = create
                    .name
                    .as_ref()
                    .map(extract_ident)
                    .unwrap_or_else(|| "idx".to_string());
                let cols: Vec<String> = create
                    .columns
                    .iter()
                    .map(|c| expr_to_column_name(&c.expr))
                    .collect();
                self.create_index(&table, &index_name, cols, create.unique)
            }
            Statement::Explain { statement, .. } => self.explain(statement.as_ref()),
            _ => Err(crate::error::MoodengError::Execution(format!(
                "unsupported statement: {stmt}"
            ))),
        }
    }

    fn track_write_txn_start(&self) {
        self.active_write_txns.fetch_add(1, Ordering::Release);
    }

    fn track_write_txn_end(&self) {
        self.active_write_txns.fetch_sub(1, Ordering::Release);
    }

    fn begin(&mut self) -> crate::error::Result<QueryResult> {
        if self.session.transaction.is_active() {
            return Err(crate::error::MoodengError::Execution(
                "transaction already active".into(),
            ));
        }
        self.session.auto_commit = false;
        self.session.transaction.begin(self.storage.wal())?;
        self.track_write_txn_start();
        Ok(QueryResult::modified(0, "BEGIN"))
    }

    fn commit(&mut self) -> crate::error::Result<QueryResult> {
        if !self.session.transaction.is_active() {
            return Err(crate::error::MoodengError::Execution(
                "no active transaction".into(),
            ));
        }
        self.session.auto_commit = false;
        self.session.transaction.commit(self.storage.wal())?;
        self.track_write_txn_end();
        Ok(QueryResult::modified(0, "COMMIT"))
    }

    fn rollback(&mut self) -> crate::error::Result<QueryResult> {
        if !self.session.transaction.is_active() {
            return Err(crate::error::MoodengError::Execution(
                "no active transaction".into(),
            ));
        }
        self.session.auto_commit = false;
        let records = self.session.transaction.rollback(self.storage.wal())?;
        for record in records {
            apply_undo(self.storage, self.indexes, self.catalog, &record)?;
        }
        self.track_write_txn_end();
        Ok(QueryResult::modified(0, "ROLLBACK"))
    }

    fn begin_write_txn(&mut self) -> crate::error::Result<u64> {
        if let Some(txn_id) = self.session.transaction.current_txn_id() {
            return Ok(txn_id);
        }
        self.session.transaction.begin(self.storage.wal())?;
        self.session.auto_commit = true;
        self.track_write_txn_start();
        Ok(self.session.transaction.current_txn_id().unwrap())
    }

    fn finish_auto_commit_if_needed(&mut self) -> crate::error::Result<()> {
        if self.session.auto_commit {
            self.session.transaction.commit(self.storage.wal())?;
            self.session.auto_commit = false;
            self.track_write_txn_end();
        }
        Ok(())
    }

    fn create_table(
        &self,
        name: &ObjectName,
        columns: &[sqlparser::ast::ColumnDef],
        constraints: &[sqlparser::ast::TableConstraint],
    ) -> crate::error::Result<QueryResult> {
        let table = extract_table_name(name);
        let lock = self.locks.ddl_lock_for(&table);
        let _guard = lock.write();
        let mut col_defs: Vec<ColumnDef> = columns
            .iter()
            .map(sql_column_to_def)
            .collect::<crate::error::Result<_>>()?;
        let pk_cols = extract_pk_from_constraints(constraints);
        for col in &mut col_defs {
            if pk_cols.iter().any(|p| p.eq_ignore_ascii_case(&col.name)) {
                col.primary_key = true;
                col.nullable = false;
            }
        }
        self.catalog.create_table(table.clone(), col_defs.clone())?;
        self.storage.ensure_table(&table);
        self.create_primary_key_index(&table, &col_defs)?;
        Ok(QueryResult::ddl(format!("CREATE TABLE {table}")))
    }

    fn create_primary_key_index(
        &self,
        table: &str,
        columns: &[ColumnDef],
    ) -> crate::error::Result<()> {
        let pk_cols: Vec<String> = columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect();

        if pk_cols.is_empty() {
            return Ok(());
        }

        let index_name = format!("{table}_pkey");
        let index = BTreeIndex::new(index_name.clone(), pk_cols.clone(), true);
        self.indexes.create_index(table, index)?;

        self.catalog.add_index(
            table,
            IndexMeta {
                name: index_name,
                columns: pk_cols,
                unique: true,
            },
        )?;

        Ok(())
    }

    fn validate_row(
        schema: &crate::catalog::TableSchema,
        row_values: &[Value],
    ) -> crate::error::Result<()> {
        for (i, col) in schema.columns.iter().enumerate() {
            let val = row_values.get(i).cloned().unwrap_or(Value::Null);
            if !col.nullable && val.is_null() {
                return Err(crate::error::MoodengError::Execution(format!(
                    "null value in column '{}' violates not-null constraint",
                    col.name
                )));
            }
        }
        Ok(())
    }

    fn drop_table(&self, table: &str) -> crate::error::Result<QueryResult> {
        let lock = self.locks.ddl_lock_for(table);
        let _guard = lock.write();
        self.catalog.drop_table(table)?;
        self.storage.drop_table(table)?;
        self.indexes.drop_table(table);
        Ok(QueryResult::ddl(format!("DROP TABLE {table}")))
    }

    fn insert(&mut self, table: &str, source: &Query) -> crate::error::Result<QueryResult> {
        let schema = self.catalog.get_table(table)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let rows = values_from_insert(source)?;
        let mut count = 0u64;
        let backup_lock = self.backup_lock;
        let _backup = backup_lock.read();
        let txn_id = self.begin_write_txn()?;

        for values in rows {
            let mut row_values = Vec::with_capacity(schema.columns.len());
            for (i, col) in schema.columns.iter().enumerate() {
                let val = values.get(i).cloned().unwrap_or(Value::Null);
                row_values.push(val.coerce_to(&col.data_type)?);
            }

            Self::validate_row(&schema, &row_values)?;

            let row_id = self
                .storage
                .insert(table, Row::new(row_values.clone()), txn_id)?;
            self.indexes
                .insert_row(table, &row_values, &col_names, row_id)?;

            if self.session.transaction.is_active() && !self.session.auto_commit {
                self.session.transaction.record(UndoRecord::Insert {
                    table: table.to_string(),
                    row_id,
                });
            }
            count += 1;
        }

        self.finish_auto_commit_if_needed()?;
        Ok(QueryResult::modified(count, format!("INSERT 0 {count}")))
    }

    fn select(&self, query: &Query) -> crate::error::Result<QueryResult> {
        let SetExpr::Select(select) = query.body.as_ref() else {
            return Err(crate::error::MoodengError::Parse("unsupported SELECT".into()));
        };

        if let Some(from) = select.from.first() {
            if !from.joins.is_empty() {
                return self.select_join(query, select, from);
            }
        }

        let table_name = select
            .from
            .first()
            .map(|twj| table_factor_name(&twj.relation))
            .ok_or_else(|| crate::error::MoodengError::Parse("missing FROM".into()))?;
        let where_clause = select.selection.clone();

        let schema = self.catalog.get_table(&table_name)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let select_items = &select.projection;

        let wildcard = select_items.iter().any(|i| matches!(i, SelectItem::Wildcard(_)));
        let output_cols = if wildcard {
            col_names.clone()
        } else {
            select_items_to_columns(select_items)
        };

        let plan = plan_scan(&table_name, where_clause.as_ref(), self.indexes, &col_names);
        let candidate_rows = match plan {
            ScanPlan::FullScan => self.storage.scan(&table_name)?,
            ScanPlan::IndexLookup {
                index_name,
                key_values,
            } => {
                let ids = self.indexes.lookup(&table_name, &index_name, &key_values);
                self.storage.fetch_by_ids(&table_name, &ids)?
            }
        };

        let mut result_rows = Vec::new();

        for (_, row) in candidate_rows {
            if let Some(w) = &where_clause {
                if !eval_where(w, &row.values, &col_names)? {
                    continue;
                }
            }
            result_rows.push(row);
        }

        self.finalize_select(query, select, output_cols, result_rows, &col_names)
    }

    fn explain(&self, stmt: &Statement) -> crate::error::Result<QueryResult> {
        let Statement::Query(query) = stmt else {
            return Err(crate::error::MoodengError::Execution(
                "EXPLAIN only supports SELECT queries".into(),
            ));
        };

        let SetExpr::Select(select) = query.body.as_ref() else {
            return Err(crate::error::MoodengError::Parse(
                "EXPLAIN only supports simple SELECT".into(),
            ));
        };

        if !select.from.first().is_some_and(|f| f.joins.is_empty()) {
            return Err(crate::error::MoodengError::Execution(
                "EXPLAIN for JOIN not yet supported".into(),
            ));
        }

        let table_name = select
            .from
            .first()
            .map(|twj| table_factor_name(&twj.relation))
            .ok_or_else(|| crate::error::MoodengError::Parse("missing FROM".into()))?;

        let schema = self.catalog.get_table(&table_name)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let plan = plan_scan(
            &table_name,
            select.selection.as_ref(),
            self.indexes,
            &col_names,
        );

        let mut lines = vec![format!("{plan} on {table_name}")];
        if let ScanPlan::IndexLookup { key_values, .. } = &plan {
            if let Some(val) = key_values.first() {
                lines.push(format!("  Index Cond: (= {val})"));
            }
        }

        Ok(QueryResult::select(
            vec!["QUERY PLAN".into()],
            vec![Row::new(vec![Value::Text(lines.join("\n"))])],
        ))
    }

    fn select_join(
        &self,
        query: &Query,
        select: &sqlparser::ast::Select,
        from: &TableWithJoins,
    ) -> crate::error::Result<QueryResult> {
        let left_table = table_factor_name(&from.relation);

        let left_schema = self.catalog.get_table(&left_table)?;
        let left_cols: Vec<String> = left_schema
            .columns
            .iter()
            .map(|c| format!("{}.{}", left_table, c.name))
            .collect();
        let left_rows = self.storage.scan(&left_table)?;

        let mut combined_cols = left_cols.clone();
        let mut joined_rows: Vec<Row> = left_rows
            .into_iter()
            .map(|(_, row)| Row::new(row.values))
            .collect();

        for join in &from.joins {
            let (right_table, on_expr) = parse_inner_join(join)?;

            let right_schema = self.catalog.get_table(&right_table)?;
            let right_cols: Vec<String> = right_schema
                .columns
                .iter()
                .map(|c| format!("{}.{}", right_table, c.name))
                .collect();
            let right_rows = self.storage.scan(&right_table)?;

            combined_cols.extend(right_cols.clone());

            let mut next = Vec::new();
            for lrow in &joined_rows {
                for (_, rrow) in &right_rows {
                    let mut values = lrow.values.clone();
                    values.extend(rrow.values.clone());
                    if eval_where(&on_expr, &values, &combined_cols)? {
                        next.push(Row::new(values));
                    }
                }
            }
            joined_rows = next;
        }

        let select_items = &select.projection;
        let wildcard = select_items.iter().any(|i| matches!(i, SelectItem::Wildcard(_)));
        let output_cols = if wildcard {
            combined_cols.clone()
        } else {
            select_items_to_columns(select_items)
        };

        let where_clause = select.selection.clone();
        let mut result_rows = Vec::new();

        for row in joined_rows {
            if let Some(w) = &where_clause {
                if !eval_where(w, &row.values, &combined_cols)? {
                    continue;
                }
            }
            result_rows.push(row);
        }

        self.finalize_select(query, select, output_cols, result_rows, &combined_cols)
    }

    fn finalize_select(
        &self,
        query: &Query,
        select: &sqlparser::ast::Select,
        mut output_cols: Vec<String>,
        mut result_rows: Vec<Row>,
        col_names: &[String],
    ) -> crate::error::Result<QueryResult> {
        if group_by_exprs(&select.group_by).is_empty()
            && !crate::query_util::is_aggregate_query(&select.projection)
        {
            // no grouping
        } else {
            let (gb_cols, gb_rows) = apply_group_by(
                &select.projection,
                &group_by_exprs(&select.group_by),
                result_rows,
                col_names,
            )?;
            if !gb_cols.is_empty() {
                output_cols = gb_cols;
            }
            result_rows = gb_rows;
        }

        if let Some(order_by) = &query.order_by {
            sort_rows(&mut result_rows, &order_by.exprs, col_names)?;
        }

        result_rows = apply_limit_offset(
            result_rows,
            query.limit.as_ref(),
            query.offset.as_ref().map(|o| &o.value),
            col_names,
        )?;

        let wildcard = select
            .projection
            .iter()
            .any(|i| matches!(i, SelectItem::Wildcard(_)));
        result_rows = project_rows(&select.projection, result_rows, col_names, wildcard)?;

        Ok(QueryResult::select(output_cols, result_rows))
    }

    fn update(
        &mut self,
        table: &str,
        assignments: &[Assignment],
        where_clause: Option<&Expr>,
    ) -> crate::error::Result<QueryResult> {
        let schema = self.catalog.get_table(table)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let all_rows = self.storage.scan(table)?;
        let mut count = 0u64;
        let backup_lock = self.backup_lock;
        let _backup = backup_lock.read();
        let txn_id = self.begin_write_txn()?;

        for (id, mut row) in all_rows {
            let should_update = match where_clause {
                Some(w) => eval_where(w, &row.values, &col_names)?,
                None => true,
            };

            if should_update {
                let handle = self.locks.row_lock(table, id);
                let _guard = handle.write();

                if self.session.transaction.is_active() && !self.session.auto_commit {
                    self.session.transaction.record(UndoRecord::Update {
                        table: table.to_string(),
                        row_id: id,
                        old_row: row.clone(),
                    });
                }

                self.indexes.remove_row(table, &row.values, &col_names, id);
                for assign in assignments {
                    let (col, val) = assignment_value(assign, &row.values, &col_names)?;
                    if let Some(idx) = self.catalog.column_index(&schema, &col) {
                        row.values[idx] = val.coerce_to(&schema.columns[idx].data_type)?;
                    }
                }

                Self::validate_row(&schema, &row.values)?;

                let expected_version = row.version;
                self.storage
                    .update(table, id, row.clone(), txn_id, expected_version)?;
                self.indexes.insert_row(table, &row.values, &col_names, id)?;
                count += 1;
            }
        }

        self.finish_auto_commit_if_needed()?;
        Ok(QueryResult::modified(count, format!("UPDATE {count}")))
    }

    fn delete(
        &mut self,
        table: &str,
        where_clause: Option<&Expr>,
    ) -> crate::error::Result<QueryResult> {
        let schema = self.catalog.get_table(table)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
        let all_rows = self.storage.scan(table)?;
        let mut count = 0u64;
        let backup_lock = self.backup_lock;
        let _backup = backup_lock.read();
        let txn_id = self.begin_write_txn()?;

        for (id, row) in all_rows {
            let should_delete = match where_clause {
                Some(w) => eval_where(w, &row.values, &col_names)?,
                None => true,
            };

            if should_delete {
                let handle = self.locks.row_lock(table, id);
                let _guard = handle.write();

                if self.session.transaction.is_active() && !self.session.auto_commit {
                    self.session.transaction.record(UndoRecord::Delete {
                        table: table.to_string(),
                        row_id: id,
                        old_row: row.clone(),
                    });
                }

                self.indexes.remove_row(table, &row.values, &col_names, id);
                self.storage.delete(table, id, txn_id)?;
                count += 1;
            }
        }

        self.finish_auto_commit_if_needed()?;
        Ok(QueryResult::modified(count, format!("DELETE {count}")))
    }

    fn create_index(
        &self,
        table: &str,
        index_name: &str,
        columns: Vec<String>,
        unique: bool,
    ) -> crate::error::Result<QueryResult> {
        let lock = self.locks.ddl_lock_for(table);
        let _guard = lock.write();
        let schema = self.catalog.get_table(table)?;
        let col_names: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();

        for col in &columns {
            if self.catalog.column_index(&schema, col).is_none() {
                return Err(crate::error::MoodengError::ColumnNotFound(col.clone()));
            }
        }

        let mut index = BTreeIndex::new(index_name.to_string(), columns.clone(), unique);
        let all_rows = self.storage.scan(table)?;
        for (id, row) in &all_rows {
            let key_vals: Vec<Value> = columns
                .iter()
                .map(|col| {
                    let i = col_names
                        .iter()
                        .position(|c| c.eq_ignore_ascii_case(col))
                        .unwrap_or(0);
                    row.values.get(i).cloned().unwrap_or(Value::Null)
                })
                .collect();
            index.insert(&key_vals, *id)?;
        }

        self.indexes.create_index(table, index)?;
        self.catalog.add_index(
            table,
            IndexMeta {
                name: index_name.to_string(),
                columns,
                unique,
            },
        )?;

        Ok(QueryResult::ddl(format!("CREATE INDEX {index_name}")))
    }
}



fn project_rows(
    select_items: &[SelectItem],
    rows: Vec<Row>,
    col_names: &[String],
    wildcard: bool,
) -> crate::error::Result<Vec<Row>> {
    rows.into_iter()
        .map(|row| {
            let projected = if wildcard {
                row.values
            } else {
                select_items
                    .iter()
                    .map(|item| match item {
                        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                            eval_expr(expr, &row.values, col_names)
                        }
                        _ => Ok(Value::Null),
                    })
                    .collect::<crate::error::Result<Vec<_>>>()?
            };
            Ok(Row::new(projected))
        })
        .collect()
}

fn group_by_exprs(group_by: &GroupByExpr) -> Vec<Expr> {
    match group_by {
        GroupByExpr::All(_) => vec![],
        GroupByExpr::Expressions(exprs, _) => exprs.clone(),
    }
}

fn parse_inner_join(join: &Join) -> crate::error::Result<(String, Expr)> {
    let table = table_factor_name(&join.relation);
    match &join.join_operator {
        JoinOperator::Inner(JoinConstraint::On(expr)) => Ok((table, expr.clone())),
        JoinOperator::Inner(JoinConstraint::None) | JoinOperator::CrossJoin => {
            Err(crate::error::MoodengError::Parse(
                "JOIN requires ON clause".into(),
            ))
        }
        _ => Err(crate::error::MoodengError::Parse(
            "only INNER JOIN supported".into(),
        )),
    }
}

fn extract_select_info(query: &Query) -> crate::error::Result<(String, Option<Expr>)> {
    match query.body.as_ref() {
        SetExpr::Select(select) => {
            let table = select
                .from
                .first()
                .map(|twj| table_factor_name(&twj.relation))
                .ok_or_else(|| crate::error::MoodengError::Parse("missing FROM".into()))?;
            Ok((table, select.selection.clone()))
        }
        _ => Err(crate::error::MoodengError::Parse(
            "unsupported query body".into(),
        )),
    }
}

fn extract_delete_table(from: &sqlparser::ast::FromTable) -> String {
    let tables = match from {
        sqlparser::ast::FromTable::WithFromKeyword(t) => t,
        sqlparser::ast::FromTable::WithoutKeyword(t) => t,
    };
    tables
        .first()
        .map(|twj| table_factor_name(&twj.relation))
        .unwrap_or_default()
}

fn expr_to_column_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(ident) => ident.value.clone(),
        _ => format!("{expr}"),
    }
}

fn table_factor_name(factor: &TableFactor) -> String {
    match factor {
        TableFactor::Table { name, .. } => extract_table_name(name),
        _ => String::new(),
    }
}
