use sqlparser::ast::{
    Assignment, BinaryOperator, ColumnDef as SqlColumnDef, DataType as SqlDataType, Expr,
    Ident, ObjectName, SelectItem, SetExpr, Statement, TableConstraint,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::types::{ColumnDef, DataType, Value};

pub fn parse_sql(sql: &str) -> crate::error::Result<Vec<Statement>> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|e| crate::error::MoodengError::Parse(e.to_string()))
}

pub fn extract_table_name(name: &ObjectName) -> String {
    name.0
        .last()
        .map(|i| i.value.clone())
        .unwrap_or_default()
}

pub fn extract_ident(name: &ObjectName) -> String {
    extract_table_name(name)
}

pub fn sql_column_to_def(col: &SqlColumnDef) -> crate::error::Result<ColumnDef> {
    let data_type = sql_type_to_data_type(&col.data_type)?;
    let mut primary_key = false;

    for option in &col.options {
        if let sqlparser::ast::ColumnOption::Unique { is_primary, .. } = &option.option {
            if *is_primary {
                primary_key = true;
            }
        }
    }

    Ok(ColumnDef {
        name: col.name.value.clone(),
        data_type,
        nullable: !col.options.iter().any(|o| {
            matches!(o.option, sqlparser::ast::ColumnOption::NotNull)
        }),
        primary_key,
    })
}

pub fn sql_type_to_data_type(dt: &SqlDataType) -> crate::error::Result<DataType> {
    match dt {
        SqlDataType::Int(_) | SqlDataType::Integer(_) => Ok(DataType::Int4),
        SqlDataType::BigInt(_) => Ok(DataType::Int8),
        SqlDataType::Float(_) => Ok(DataType::Float4),
        SqlDataType::Double => Ok(DataType::Float8),
        SqlDataType::Text | SqlDataType::String(_) => Ok(DataType::Text),
        SqlDataType::Boolean => Ok(DataType::Bool),
        SqlDataType::Timestamp(_, _) => Ok(DataType::Timestamp),
        SqlDataType::JSON | SqlDataType::JSONB => Ok(DataType::Json),
        SqlDataType::Varchar(size) | SqlDataType::CharacterVarying(size) => {
            let len = match size {
                Some(sqlparser::ast::CharacterLength::IntegerLength { length, .. }) => *length as usize,
                Some(sqlparser::ast::CharacterLength::Max) => 65535,
                None => 255,
            };
            Ok(DataType::Varchar(len))
        }
        other => {
            if let Some(name) = dt_name(other) {
                DataType::from_sql(name).ok_or_else(|| {
                    crate::error::MoodengError::Parse(format!("unsupported type: {name}"))
                })
            } else {
                Err(crate::error::MoodengError::Parse(format!(
                    "unsupported type: {other:?}"
                )))
            }
        }
    }
}

fn dt_name(dt: &SqlDataType) -> Option<&str> {
    match dt {
        SqlDataType::Custom(name, _) => name.0.last().map(|i| i.value.as_str()),
        _ => None,
    }
}

pub fn eval_expr(expr: &Expr, row: &[Value], columns: &[String]) -> crate::error::Result<Value> {
    match expr {
        Expr::Value(v) => Ok(Value::from_sql_literal(v)),
        Expr::Identifier(Ident { value, .. }) => column_value(value, row, columns),
        Expr::CompoundIdentifier(parts) => {
            let col = parts.last().map(|i| i.value.as_str()).unwrap_or("");
            column_value(col, row, columns)
        }
        Expr::BinaryOp { left, op, right } => {
            let l = eval_expr(left, row, columns)?;
            let r = eval_expr(right, row, columns)?;
            eval_binary(&l, op, &r)
        }
        Expr::UnaryOp { op, expr } => {
            let v = eval_expr(expr, row, columns)?;
            match op {
                sqlparser::ast::UnaryOperator::Not => match v {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    _ => Ok(Value::Bool(false)),
                },
                sqlparser::ast::UnaryOperator::Minus => match v {
                    Value::Int4(n) => Ok(Value::Int4(-n)),
                    Value::Int8(n) => Ok(Value::Int8(-n)),
                    Value::Float8(n) => Ok(Value::Float8(-n)),
                    other => Ok(other),
                },
                _ => eval_expr(expr, row, columns),
            }
        }
        Expr::IsNull(expr) => {
            let v = eval_expr(expr, row, columns)?;
            Ok(Value::Bool(v.is_null()))
        }
        Expr::IsNotNull(expr) => {
            let v = eval_expr(expr, row, columns)?;
            Ok(Value::Bool(!v.is_null()))
        }
        Expr::Nested(inner) => eval_expr(inner, row, columns),
        Expr::Function(func) => eval_function(func, row, columns),
        _ => Ok(Value::Null),
    }
}

fn column_value(col: &str, row: &[Value], columns: &[String]) -> crate::error::Result<Value> {
    let idx = columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case(col))
        .ok_or_else(|| crate::error::MoodengError::ColumnNotFound(col.into()))?;
    Ok(row.get(idx).cloned().unwrap_or(Value::Null))
}

fn eval_binary(l: &Value, op: &BinaryOperator, r: &Value) -> crate::error::Result<Value> {
    use BinaryOperator::*;
    if matches!(l, Value::Null) || matches!(r, Value::Null) {
        match op {
            Eq | NotEq | Lt | LtEq | Gt | GtEq => return Ok(Value::Bool(false)),
            And | Or => {}
            _ => return Ok(Value::Null),
        }
    }

    match (l, op, r) {
        (Value::Int4(a), Plus, Value::Int4(b)) => Ok(Value::Int4(a + b)),
        (Value::Int8(a), Plus, Value::Int8(b)) => Ok(Value::Int8(a + b)),
        (Value::Int4(a), Minus, Value::Int8(b)) => Ok(Value::Int8(*a as i64 - b)),
        (Value::Int8(a), Multiply, Value::Int8(b)) => Ok(Value::Int8(a * b)),
        (Value::Float8(a), Plus, Value::Float8(b)) => Ok(Value::Float8(a + b)),
        (Value::Float8(a), Minus, Value::Float8(b)) => Ok(Value::Float8(a - b)),
        (Value::Text(a), StringConcat, Value::Text(b)) => Ok(Value::Text(format!("{a}{b}"))),
        (a, Eq, b) => Ok(Value::Bool(values_equal(a, b))),
        (a, NotEq, b) => Ok(Value::Bool(!values_equal(a, b))),
        (a, Lt, b) => Ok(Value::Bool(compare_ints(a, b).is_some_and(|o| o == std::cmp::Ordering::Less))),
        (a, LtEq, b) => Ok(Value::Bool(compare_ints(a, b).is_some_and(|o| o != std::cmp::Ordering::Greater))),
        (a, Gt, b) => Ok(Value::Bool(compare_ints(a, b).is_some_and(|o| o == std::cmp::Ordering::Greater))),
        (a, GtEq, b) => Ok(Value::Bool(compare_ints(a, b).is_some_and(|o| o != std::cmp::Ordering::Less))),
        (Value::Bool(a), And, Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (Value::Bool(a), Or, Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
        _ => Ok(Value::Null),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    if let (Some(ai), Some(bi)) = (as_i64(a), as_i64(b)) {
        return ai == bi;
    }
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Float8(x), Value::Float8(y)) => (x - y).abs() < f64::EPSILON,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int4(n) => Some(*n as i64),
        Value::Int8(n) => Some(*n),
        _ => None,
    }
}

fn compare_ints(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    Some(as_i64(a)?.cmp(&as_i64(b)?))
}

fn eval_function(
    func: &sqlparser::ast::Function,
    row: &[Value],
    columns: &[String],
) -> crate::error::Result<Value> {
    let name = func.name.to_string().to_uppercase();
    let args = match &func.args {
        sqlparser::ast::FunctionArguments::List(list) => &list.args,
        _ => return Ok(Value::Null),
    };

    match name.as_str() {
        "COUNT" => Ok(Value::Int8(1)),
        "UPPER" => {
            if let Some(arg) = args.first() {
                if let sqlparser::ast::FunctionArg::Unnamed(
                    sqlparser::ast::FunctionArgExpr::Expr(expr),
                ) = arg
                {
                    if let Value::Text(s) = eval_expr(expr, row, columns)? {
                        return Ok(Value::Text(s.to_uppercase()));
                    }
                }
            }
            Ok(Value::Null)
        }
        "LOWER" => {
            if let Some(arg) = args.first() {
                if let sqlparser::ast::FunctionArg::Unnamed(
                    sqlparser::ast::FunctionArgExpr::Expr(expr),
                ) = arg
                {
                    if let Value::Text(s) = eval_expr(expr, row, columns)? {
                        return Ok(Value::Text(s.to_lowercase()));
                    }
                }
            }
            Ok(Value::Null)
        }
        _ => Ok(Value::Null),
    }
}

pub fn eval_where(expr: &Expr, row: &[Value], columns: &[String]) -> crate::error::Result<bool> {
    match eval_expr(expr, row, columns)? {
        Value::Bool(b) => Ok(b),
        Value::Null => Ok(false),
        _ => Ok(true),
    }
}

pub fn assignment_value(
    assign: &Assignment,
    row: &[Value],
    columns: &[String],
) -> crate::error::Result<(String, Value)> {
    use sqlparser::ast::AssignmentTarget;
    let col = match &assign.target {
        AssignmentTarget::ColumnName(name) => extract_table_name(name),
        AssignmentTarget::Tuple(names) => names
            .first()
            .map(extract_table_name)
            .unwrap_or_default(),
    };
    let val = eval_expr(&assign.value, row, columns)?;
    Ok((col, val))
}

pub fn extract_pk_from_constraints(constraints: &[TableConstraint]) -> Vec<String> {
    constraints
        .iter()
        .filter_map(|c| {
            if let TableConstraint::PrimaryKey { columns, .. } = c {
                Some(columns.iter().map(|i| i.value.clone()).collect::<Vec<_>>())
            } else {
                None
            }
        })
        .flatten()
        .collect()
}

pub fn values_from_insert(
    source: &sqlparser::ast::Query,
) -> crate::error::Result<Vec<Vec<Value>>> {
    match source.body.as_ref() {
        SetExpr::Values(values) => values
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|expr| match expr {
                        Expr::Value(v) => Ok(Value::from_sql_literal(v)),
                        _ => Ok(Value::Null),
                    })
                    .collect()
            })
            .collect(),
        _ => Err(crate::error::MoodengError::Parse(
            "only VALUES inserts supported".into(),
        )),
    }
}

pub fn select_items_to_columns(items: &[SelectItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            SelectItem::Wildcard(_) => None,
            SelectItem::UnnamedExpr(expr) => Some(expr_to_string(expr)),
            SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
            _ => None,
        })
        .collect()
}

fn expr_to_string(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(Ident { value, .. }) => value.clone(),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| i.value.clone()).unwrap_or_default(),
        _ => format!("{expr}"),
    }
}