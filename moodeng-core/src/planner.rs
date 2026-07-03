use sqlparser::ast::{BinaryOperator, Expr, Ident};

use crate::index::IndexManager;
use crate::types::Value;

#[derive(Debug, Clone)]
pub enum ScanPlan {
    FullScan,
    IndexLookup {
        index_name: String,
        key_values: Vec<Value>,
    },
}

pub fn plan_scan(
    table: &str,
    where_clause: Option<&Expr>,
    indexes: &IndexManager,
    col_names: &[String],
) -> ScanPlan {
    let Some(expr) = where_clause else {
        return ScanPlan::FullScan;
    };

    let Some((col, val)) = extract_equality(expr) else {
        return ScanPlan::FullScan;
    };

    if col_names.iter().all(|c| !c.eq_ignore_ascii_case(&col)) {
        return ScanPlan::FullScan;
    }

    if let Some(index_name) = indexes.find_index_for_column(table, &col) {
        ScanPlan::IndexLookup {
            index_name,
            key_values: vec![val],
        }
    } else {
        ScanPlan::FullScan
    }
}

fn extract_equality(expr: &Expr) -> Option<(String, Value)> {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => match (left.as_ref(), right.as_ref()) {
            (Expr::Identifier(Ident { value, .. }), Expr::Value(v)) => {
                Some((value.clone(), Value::from_sql_literal(v)))
            }
            (Expr::Value(v), Expr::Identifier(Ident { value, .. })) => {
                Some((value.clone(), Value::from_sql_literal(v)))
            }
            _ => None,
        },
        Expr::Nested(inner) => extract_equality(inner),
        _ => None,
    }
}
