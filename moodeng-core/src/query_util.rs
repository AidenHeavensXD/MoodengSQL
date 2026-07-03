use sqlparser::ast::{Expr, Function, FunctionArguments, OrderByExpr, SelectItem};

use crate::parser::eval_expr;
use crate::types::{Row, Value};

pub fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Int4(x), Value::Int4(y)) => x.cmp(y),
        (Value::Int8(x), Value::Int8(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Float8(x), Value::Float8(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => a.to_display_string().cmp(&b.to_display_string()),
    }
}

pub fn sort_rows(
    rows: &mut [Row],
    order_by: &[OrderByExpr],
    col_names: &[String],
) -> crate::error::Result<()> {
    if order_by.is_empty() {
        return Ok(());
    }

    rows.sort_by(|a, b| {
        for ob in order_by {
            let asc = ob.asc.unwrap_or(true);
            let va = eval_expr(&ob.expr, &a.values, col_names).unwrap_or(Value::Null);
            let vb = eval_expr(&ob.expr, &b.values, col_names).unwrap_or(Value::Null);
            let ord = compare_values(&va, &vb);
            if ord != std::cmp::Ordering::Equal {
                return if asc { ord } else { ord.reverse() };
            }
        }
        std::cmp::Ordering::Equal
    });
    Ok(())
}

pub fn apply_limit_offset(
    rows: Vec<Row>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    col_names: &[String],
) -> crate::error::Result<Vec<Row>> {
    let off = offset
        .map(|e| expr_to_usize(e, col_names))
        .transpose()?
        .unwrap_or(0);
    let lim = limit
        .map(|e| expr_to_usize(e, col_names))
        .transpose()?;

    let rows: Vec<_> = rows.into_iter().skip(off).collect();
    Ok(match lim {
        Some(n) => rows.into_iter().take(n).collect(),
        None => rows,
    })
}

fn expr_to_usize(expr: &Expr, col_names: &[String]) -> crate::error::Result<usize> {
    let empty = &[];
    let val = eval_expr(expr, empty, col_names)?;
    match val {
        Value::Int4(n) if n >= 0 => Ok(n as usize),
        Value::Int8(n) if n >= 0 => Ok(n as usize),
        other => Err(crate::error::MoodengError::Execution(format!(
            "invalid limit/offset: {other}"
        ))),
    }
}

pub fn is_aggregate_query(items: &[SelectItem]) -> bool {
    items.iter().any(|item| match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => is_agg_expr(expr),
        _ => false,
    })
}

fn is_agg_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Function(f) => is_agg_function(f),
        _ => false,
    }
}

fn is_agg_function(func: &Function) -> bool {
    matches!(
        func.name.to_string().to_uppercase().as_str(),
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX"
    )
}

pub fn apply_group_by(
    items: &[SelectItem],
    group_exprs: &[Expr],
    rows: Vec<Row>,
    col_names: &[String],
) -> crate::error::Result<(Vec<String>, Vec<Row>)> {
    if group_exprs.is_empty() && !is_aggregate_query(items) {
        return Ok((Vec::new(), rows));
    }

    use std::collections::HashMap;

    let mut groups: HashMap<String, Vec<Row>> = HashMap::new();

    for row in rows {
        let key: Vec<String> = group_exprs
            .iter()
            .map(|e| eval_expr(e, &row.values, col_names).map(|v| v.to_display_string()))
            .collect::<crate::error::Result<_>>()?;
        groups.entry(key.join("\x1f")).or_default().push(row);
    }

    let output_cols = if items.iter().any(|i| matches!(i, SelectItem::Wildcard(_))) {
        col_names.to_vec()
    } else {
        items
            .iter()
            .filter_map(|item| match item {
                SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
                SelectItem::UnnamedExpr(expr) => Some(format!("{expr}")),
                _ => None,
            })
            .collect()
    };

    let mut result = Vec::new();
    for group_rows in groups.into_values() {
        if items.iter().any(|i| matches!(i, SelectItem::Wildcard(_))) {
            if let Some(first) = group_rows.first() {
                result.push(Row::new(first.values.clone()));
                continue;
            }
        }

        let values: Vec<Value> = items
            .iter()
            .filter_map(|item| match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    Some(eval_grouped_expr(expr, &group_rows, col_names))
                }
                _ => None,
            })
            .collect::<crate::error::Result<Vec<_>>>()?;

        result.push(Row::new(values));
    }

    Ok((output_cols, result))
}

fn eval_grouped_expr(
    expr: &Expr,
    group_rows: &[Row],
    col_names: &[String],
) -> crate::error::Result<Value> {
    if is_agg_expr(expr) {
        Ok(eval_agg_expr(expr, group_rows, col_names)?
            .into_iter()
            .next()
            .unwrap_or(Value::Null))
    } else {
        eval_expr(
            expr,
            &group_rows.first().map(|r| r.values.as_slice()).unwrap_or(&[]),
            col_names,
        )
    }
}

fn eval_agg_expr(
    expr: &Expr,
    group_rows: &[Row],
    col_names: &[String],
) -> crate::error::Result<Vec<Value>> {
    match expr {
        Expr::Function(func) => {
            let name = func.name.to_string().to_uppercase();
            let args = match &func.args {
                FunctionArguments::List(list) => list.args.as_slice(),
                _ => &[] as &[_],
            };

            match name.as_str() {
                "COUNT" => Ok(vec![Value::Int8(group_rows.len() as i64)]),
                "SUM" => {
                    let col = function_values(args, group_rows, col_names)?;
                    let sum: f64 = col.iter().filter_map(value_as_f64).sum();
                    Ok(vec![Value::Float8(sum)])
                }
                "AVG" => {
                    let col = function_values(args, group_rows, col_names)?;
                    let nums: Vec<f64> = col.iter().filter_map(value_as_f64).collect();
                    let avg = if nums.is_empty() {
                        0.0
                    } else {
                        nums.iter().sum::<f64>() / nums.len() as f64
                    };
                    Ok(vec![Value::Float8(avg)])
                }
                "MIN" => {
                    let col = function_values(args, group_rows, col_names)?;
                    Ok(vec![col
                        .iter()
                        .min_by(|a, b| compare_values(a, b))
                        .cloned()
                        .unwrap_or(Value::Null)])
                }
                "MAX" => {
                    let col = function_values(args, group_rows, col_names)?;
                    Ok(vec![col
                        .iter()
                        .max_by(|a, b| compare_values(a, b))
                        .cloned()
                        .unwrap_or(Value::Null)])
                }
                _ => Ok(vec![eval_expr(expr, &group_rows[0].values, col_names)?]),
            }
        }
        _ => Ok(vec![eval_expr(expr, &group_rows[0].values, col_names)?]),
    }
}

fn function_values(
    args: &[sqlparser::ast::FunctionArg],
    group_rows: &[Row],
    col_names: &[String],
) -> crate::error::Result<Vec<Value>> {
    if let Some(arg) = args.first() {
        if let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Wildcard) =
            arg
        {
            return Ok(vec![]);
        }
        if let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) =
            arg
        {
            return group_rows
                .iter()
                .map(|r| eval_expr(expr, &r.values, col_names))
                .collect();
        }
    }
    Ok(vec![])
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int4(n) => Some(*n as f64),
        Value::Int8(n) => Some(*n as f64),
        Value::Float4(n) => Some(*n as f64),
        Value::Float8(n) => Some(*n),
        _ => None,
    }
}

pub fn substitute_params(sql: &str, params: &[Option<String>]) -> String {
    let mut result = sql.to_string();
    for (i, param) in params.iter().enumerate().rev() {
        let placeholder = format!("${}", i + 1);
        let replacement = param
            .as_ref()
            .map(|v| {
                if v.eq_ignore_ascii_case("null") {
                    "NULL".to_string()
                } else if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    v.clone()
                } else {
                    format!("'{v}'")
                }
            })
            .unwrap_or_else(|| "NULL".to_string());
        result = result.replace(&placeholder, &replacement);
    }
    result
}
