use bytes::{Buf, BufMut, BytesMut};
use moodeng_core::{substitute_params, Database, QueryResult};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::session::ConnectionSession;

pub async fn handle_connection(mut stream: TcpStream, db: Arc<Database>) -> anyhow::Result<()> {
    let len = stream.read_i32().await? as usize;
    let mut buf = vec![0u8; len.saturating_sub(4)];
    stream.read_exact(&mut buf).await?;
    let mut cursor = &buf[..];
    let _version = cursor.get_i32();

    send_message(&mut stream, b'R', |b| {
        b.put_i32(8);
        b.put_i32(0);
    })
    .await?;

    for (k, v) in [
        ("server_version", "16.0-moodeng"),
        ("server_encoding", "UTF8"),
        ("client_encoding", "UTF8"),
    ] {
        send_message(&mut stream, b'S', |b| {
            b.put_slice(k.as_bytes());
            b.put_u8(0);
            b.put_slice(v.as_bytes());
            b.put_u8(0);
        })
        .await?;
    }

    send_message(&mut stream, b'Z', |b| b.put_u8(b'I')).await?;

    let mut conn = ConnectionSession::new();

    loop {
        let msg_type = match stream.read_u8().await {
            Ok(t) => t,
            Err(_) => break,
        };
        let len = stream.read_i32().await? as usize;
        let mut payload = vec![0u8; len.saturating_sub(4)];
        if !payload.is_empty() {
            stream.read_exact(&mut payload).await?;
        }

        match msg_type {
            b'Q' => {
                let sql = read_cstring(&payload);
                if sql.is_empty() {
                    continue;
                }
                run_sql(&mut stream, &db, &mut conn, &sql).await?;
                send_ready(&mut stream, &conn).await?;
            }
            b'P' => handle_parse(&mut conn, &payload)?,
            b'B' => handle_bind(&mut conn, &payload)?,
            b'E' => {
                handle_execute(&mut stream, &db, &mut conn, &payload).await?;
            }
            b'S' => {
                conn.in_error = false;
                send_ready(&mut stream, &conn).await?;
            }
            b'X' => break,
            _ => {}
        }
    }

    Ok(())
}

async fn run_sql(
    stream: &mut TcpStream,
    db: &Arc<Database>,
    conn: &mut ConnectionSession,
    sql: &str,
) -> anyhow::Result<()> {
    match db.execute_session(&mut conn.session, sql) {
        Ok(result) => {
            conn.in_error = false;
            send_query_result(stream, &result).await?;
        }
        Err(e) => {
            conn.in_error = true;
            send_error(stream, &e.to_string()).await?;
        }
    }
    Ok(())
}

fn handle_parse(conn: &mut ConnectionSession, payload: &[u8]) -> anyhow::Result<()> {
    let mut cur = payload;
    let name = read_cstring_buf(&mut cur);
    let sql = read_cstring_buf(&mut cur);
    if name.is_empty() {
        conn.last_statement = Some(sql);
    } else {
        conn.prepared.insert(name, sql);
    }
    Ok(())
}

fn handle_bind(conn: &mut ConnectionSession, payload: &[u8]) -> anyhow::Result<()> {
    let mut cur = payload;
    let _portal = read_cstring_buf(&mut cur);
    let statement = read_cstring_buf(&mut cur);

    if cur.remaining() >= 2 {
        let _n_formats = cur.get_i16();
    }

    if cur.remaining() >= 2 {
        let n_params = cur.get_i16();
        let mut params = Vec::new();
        for _ in 0..n_params {
            if cur.remaining() >= 4 {
                let len = cur.get_i32();
                if len < 0 {
                    params.push(None);
                } else {
                    let len = len as usize;
                    if cur.remaining() >= len {
                        let val = String::from_utf8_lossy(&cur[..len]).to_string();
                        cur.advance(len);
                        params.push(Some(val));
                    }
                }
            }
        }
        conn.portal_params = params;
    }

    if statement.is_empty() {
        // use last unnamed statement
    } else if let Some(sql) = conn.prepared.get(&statement) {
        conn.last_statement = Some(sql.clone());
    }

    Ok(())
}

async fn handle_execute(
    stream: &mut TcpStream,
    db: &Arc<Database>,
    conn: &mut ConnectionSession,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut cur = payload;
    let _portal = read_cstring_buf(&mut cur);

    let sql = conn.last_statement.clone().unwrap_or_default();
    if sql.is_empty() {
        return Ok(());
    }

    let final_sql = substitute_params(&sql, &conn.portal_params);
    run_sql(stream, db, conn, &final_sql).await?;
    Ok(())
}

async fn send_ready(stream: &mut TcpStream, conn: &ConnectionSession) -> anyhow::Result<()> {
    let status = conn.ready_status();
    send_message(stream, b'Z', move |b| b.put_u8(status)).await
}

async fn send_query_result(stream: &mut TcpStream, result: &QueryResult) -> anyhow::Result<()> {
    if result.rows.is_empty() {
        let tag = result
            .message
            .clone()
            .unwrap_or_else(|| format!("{} 0", result.rows_affected));
        send_message(stream, b'C', |b| {
            b.put_slice(tag.as_bytes());
            b.put_u8(0);
        })
        .await?;
        return Ok(());
    }

    send_message(stream, b'T', |b| {
        b.put_i16(result.columns.len() as i16);
        for col in &result.columns {
            b.put_slice(col.as_bytes());
            b.put_u8(0);
            b.put_i32(0);
            b.put_i16(0);
            b.put_i32(-1);
            b.put_i16(0);
        }
    })
    .await?;

    for row in &result.rows {
        send_message(stream, b'D', |b| {
            b.put_i16(row.values.len() as i16);
            for val in &row.values {
                if val.is_null() {
                    b.put_i32(-1);
                } else {
                    let s = val.to_display_string();
                    b.put_i32(s.len() as i32);
                    b.put_slice(s.as_bytes());
                }
            }
        })
        .await?;
    }

    send_message(stream, b'C', |b| {
        let tag = format!("SELECT {}", result.rows.len());
        b.put_slice(tag.as_bytes());
        b.put_u8(0);
    })
    .await?;

    Ok(())
}

async fn send_error(stream: &mut TcpStream, msg: &str) -> anyhow::Result<()> {
    send_message(stream, b'E', |b| {
        b.put_u8(b'M');
        b.put_slice(msg.as_bytes());
        b.put_u8(0);
        b.put_u8(0);
    })
    .await?;
    Ok(())
}

async fn send_message<F>(stream: &mut TcpStream, tag: u8, fill: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut BytesMut),
{
    let mut body = BytesMut::new();
    fill(&mut body);
    let len = (body.len() + 4) as i32;
    stream.write_u8(tag).await?;
    stream.write_i32(len).await?;
    stream.write_all(&body).await?;
    Ok(())
}

fn read_cstring(payload: &[u8]) -> String {
    let mut cur = payload;
    read_cstring_buf(&mut cur)
}

fn read_cstring_buf(cur: &mut &[u8]) -> String {
    if let Some(end) = cur.iter().position(|&b| b == 0) {
        let s = String::from_utf8_lossy(&cur[..end]).to_string();
        *cur = &cur[end + 1..];
        s
    } else {
        String::new()
    }
}
