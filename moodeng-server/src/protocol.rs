use bytes::{Buf, BufMut, BytesMut};
use moodeng_core::{Database, QueryResult};
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

    send_message(&mut stream, b'Z', |b| {
        b.put_u8(b'I');
    })
    .await?;

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
                let sql = String::from_utf8_lossy(&payload[..payload.len().saturating_sub(1)])
                    .trim()
                    .to_string();
                if sql.is_empty() {
                    continue;
                }
                match db.execute_session(&mut conn.session, &sql) {
                    Ok(result) => send_query_result(&mut stream, &result).await?,
                    Err(e) => send_error(&mut stream, &e.to_string()).await?,
                }
            }
            b'X' => break,
            _ => {}
        }

        send_message(&mut stream, b'Z', |b| b.put_u8(b'I')).await?;
    }

    Ok(())
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
