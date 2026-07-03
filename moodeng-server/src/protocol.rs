use bytes::{Buf, BufMut, BytesMut};
use moodeng_core::{substitute_params, Database, QueryResult};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use crate::auth::AuthConfig;
use crate::scram::{ScramSession, SCRAM_SHA256};
use crate::session::ConnectionSession;
use crate::tls::TlsSettings;

pub const SSL_REQUEST_CODE: i32 = 80877103;

/// Reader that serves bytes from an initial prefix before delegating to the inner stream.
struct PrefixedStream<S> {
    inner: S,
    prefix: Vec<u8>,
    prefix_pos: usize,
}

impl<S> PrefixedStream<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            prefix_pos: 0,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.prefix_pos < self.prefix.len() {
            let remaining = &self.prefix[self.prefix_pos..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.prefix_pos += to_copy;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

enum ClientStream {
    Plain(PrefixedStream<TcpStream>),
    Tls(TlsStream<TcpStream>),
}

impl AsyncRead for ClientStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            ClientStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            ClientStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ClientStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            ClientStream::Plain(s) => Pin::new(s).poll_write(cx, data),
            ClientStream::Tls(s) => Pin::new(s).poll_write(cx, data),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            ClientStream::Plain(s) => Pin::new(s).poll_flush(cx),
            ClientStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            ClientStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            ClientStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

pub async fn handle_connection(
    stream: TcpStream,
    db: Arc<Database>,
    auth: Arc<AuthConfig>,
    tls: TlsSettings,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
) -> anyhow::Result<()> {
    let (client, encrypted) = negotiate_stream(stream, &tls, tls_acceptor.as_ref()).await?;
    run_session(client, db, auth, encrypted).await
}

async fn negotiate_stream(
    mut stream: TcpStream,
    tls: &TlsSettings,
    acceptor: Option<&Arc<TlsAcceptor>>,
) -> anyhow::Result<(ClientStream, bool)> {
    let len = stream.read_i32().await? as usize;
    if len == 8 {
        let code = stream.read_i32().await?;
        if code == SSL_REQUEST_CODE {
            if let Some(acc) = acceptor {
                stream.write_u8(b'S').await?;
                let tls_stream = acc.accept(stream).await?;
                return Ok((ClientStream::Tls(tls_stream), true));
            }
            stream.write_u8(b'N').await?;
            let startup_len = stream.read_i32().await? as usize;
            let mut startup_body = vec![0u8; startup_len.saturating_sub(4)];
            if !startup_body.is_empty() {
                stream.read_exact(&mut startup_body).await?;
            }
            let mut prefix = Vec::with_capacity(4 + startup_body.len());
            prefix.extend_from_slice(&(startup_len as i32).to_be_bytes());
            prefix.extend_from_slice(&startup_body);
            return Ok((
                ClientStream::Plain(PrefixedStream::new(stream, prefix)),
                false,
            ));
        }
        anyhow::bail!("unexpected pre-startup message code {code}");
    }

    if tls.require_tls {
        send_error_plain(&mut stream, "SSL required").await?;
        anyhow::bail!("plaintext connection rejected: require_tls=true");
    }

    let mut startup_body = vec![0u8; len.saturating_sub(4)];
    if !startup_body.is_empty() {
        stream.read_exact(&mut startup_body).await?;
    }
    let mut prefix = Vec::with_capacity(4 + startup_body.len());
    prefix.extend_from_slice(&(len as i32).to_be_bytes());
    prefix.extend_from_slice(&startup_body);
    Ok((
        ClientStream::Plain(PrefixedStream::new(stream, prefix)),
        false,
    ))
}

async fn run_session(
    mut stream: ClientStream,
    db: Arc<Database>,
    auth: Arc<AuthConfig>,
    encrypted: bool,
) -> anyhow::Result<()> {
    let len = stream.read_i32().await? as usize;
    let mut buf = vec![0u8; len.saturating_sub(4)];
    stream.read_exact(&mut buf).await?;
    let mut cursor = &buf[..];
    let _version = cursor.get_i32();

    if auth.required() && !encrypted && auth.require_tls_for_password && !auth.scram_available() {
        send_error(&mut stream, "password authentication requires TLS").await?;
        return Ok(());
    }

    if auth.required() {
        let ok = authenticate(&mut stream, &auth).await?;
        if !ok {
            return Ok(());
        }
    }

    send_message(&mut stream, b'R', |b| {
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

async fn authenticate<S>(stream: &mut S, auth: &AuthConfig) -> anyhow::Result<bool>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if auth.scram_available() {
        return authenticate_scram(stream, auth).await;
    }
    authenticate_cleartext(stream, auth).await
}

async fn authenticate_cleartext<S>(stream: &mut S, auth: &AuthConfig) -> anyhow::Result<bool>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_message(stream, b'R', |b| {
        b.put_i32(3);
    })
    .await?;

    let msg_type = stream.read_u8().await?;
    if msg_type != b'p' {
        send_error(stream, "expected password message").await?;
        return Ok(false);
    }
    let pw_len = stream.read_i32().await? as usize;
    let mut pw_buf = vec![0u8; pw_len.saturating_sub(4)];
    if !pw_buf.is_empty() {
        stream.read_exact(&mut pw_buf).await?;
    }
    let password = read_cstring(&pw_buf);
    if !auth.verify(&password) {
        send_error(stream, "authentication failed").await?;
        return Ok(false);
    }
    Ok(true)
}

async fn authenticate_scram<S>(stream: &mut S, auth: &AuthConfig) -> anyhow::Result<bool>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_message(stream, b'R', |b| {
        b.put_i32(10);
        b.put_slice(format!("{SCRAM_SHA256}\0\0").as_bytes());
    })
    .await?;

    let (mechanism, client_first) = read_sasl_message(stream).await?;
    if !mechanism.is_empty() && mechanism != SCRAM_SHA256 {
        send_error(stream, "unsupported SASL mechanism").await?;
        return Ok(false);
    }
    let client_first = String::from_utf8(client_first)
        .map_err(|e| anyhow::anyhow!("invalid SCRAM client-first encoding: {e}"))?;

    let creds = auth
        .scram_credentials
        .clone()
        .ok_or_else(|| anyhow::anyhow!("SCRAM credentials not configured"))?;
    let (session, server_first) = ScramSession::start(creds, &client_first)?;

    send_message(stream, b'R', |b| {
        b.put_i32(11);
        b.put_slice(server_first.as_bytes());
        b.put_u8(0);
    })
    .await?;

    let (_, client_final_bytes) = read_sasl_message(stream).await?;
    let client_final = String::from_utf8(client_final_bytes)
        .map_err(|e| anyhow::anyhow!("invalid SCRAM client-final encoding: {e}"))?;

    match session.finish(&client_final) {
        Ok(server_final) => {
            send_message(stream, b'R', |b| {
                b.put_i32(12);
                b.put_slice(server_final.as_bytes());
                b.put_u8(0);
            })
            .await?;
            Ok(true)
        }
        Err(e) => {
            send_error(stream, &format!("authentication failed: {e}")).await?;
            Ok(false)
        }
    }
}

async fn read_sasl_message<S>(stream: &mut S) -> anyhow::Result<(String, Vec<u8>)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let msg_type = stream.read_u8().await?;
    if msg_type != b'p' {
        anyhow::bail!("expected SASL password message, got {msg_type}");
    }
    let msg_len = stream.read_i32().await? as usize;
    let mut payload = vec![0u8; msg_len.saturating_sub(4)];
    if !payload.is_empty() {
        stream.read_exact(&mut payload).await?;
    }
    let mut cur = &payload[..];
    let mechanism = read_cstring_buf(&mut cur);
    if cur.remaining() < 4 {
        anyhow::bail!("truncated SASL payload");
    }
    let data_len = cur.get_i32();
    if data_len < 0 {
        return Ok((mechanism, Vec::new()));
    }
    let data_len = data_len as usize;
    if cur.remaining() < data_len {
        anyhow::bail!("truncated SASL response data");
    }
    let data = cur[..data_len].to_vec();
    Ok((mechanism, data))
}

async fn run_sql<S>(
    stream: &mut S,
    db: &Arc<Database>,
    conn: &mut ConnectionSession,
    sql: &str,
) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
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

    if !statement.is_empty() {
        if let Some(sql) = conn.prepared.get(&statement) {
            conn.last_statement = Some(sql.clone());
        }
    }

    Ok(())
}

async fn handle_execute<S>(
    stream: &mut S,
    db: &Arc<Database>,
    conn: &mut ConnectionSession,
    payload: &[u8],
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
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

async fn send_ready<S>(stream: &mut S, conn: &ConnectionSession) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let status = conn.ready_status();
    send_message(stream, b'Z', move |b| b.put_u8(status)).await
}

async fn send_query_result<S>(stream: &mut S, result: &QueryResult) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
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

async fn send_error<S>(stream: &mut S, msg: &str) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    send_message(stream, b'E', |b| {
        b.put_u8(b'M');
        b.put_slice(msg.as_bytes());
        b.put_u8(0);
        b.put_u8(0);
    })
    .await?;
    Ok(())
}

async fn send_error_plain(stream: &mut TcpStream, msg: &str) -> anyhow::Result<()> {
    send_message(stream, b'E', |b| {
        b.put_u8(b'M');
        b.put_slice(msg.as_bytes());
        b.put_u8(0);
        b.put_u8(0);
    })
    .await?;
    Ok(())
}

async fn send_message<S, F>(stream: &mut S, tag: u8, fill: F) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{hash_password, AuthConfig};
    use crate::tls::test_util::generate_test_tls;
    use tokio_rustls::rustls::RootCertStore;
    use tokio_rustls::TlsConnector;

    async fn spawn_test_server(
        auth: Arc<AuthConfig>,
        tls: TlsSettings,
        tls_acceptor: Option<Arc<TlsAcceptor>>,
    ) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let db = Arc::new(Database::in_memory().unwrap());
                let auth = Arc::clone(&auth);
                let tls = tls.clone();
                let acc = tls_acceptor.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, db, auth, tls, acc).await;
                });
            }
        });
        addr
    }

    fn startup_packet() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&196608i32.to_be_bytes());
        body.extend_from_slice(b"user\0moodeng\0");
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        pkt.extend_from_slice(&body);
        pkt
    }

    fn ssl_request_packet() -> Vec<u8> {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&8i32.to_be_bytes());
        pkt.extend_from_slice(&SSL_REQUEST_CODE.to_be_bytes());
        pkt
    }

    #[tokio::test]
    async fn tls_handshake_succeeds() {
        let dir = std::env::temp_dir().join(format!("moodeng_tls_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let test = generate_test_tls(&dir);
        let acceptor = Arc::new(TlsAcceptor::from(test.server_config.clone()));
        let auth = Arc::new(AuthConfig::default());
        let addr = spawn_test_server(auth, test.settings.clone(), Some(acceptor)).await;

        let mut root_store = RootCertStore::empty();
        let cert_pem = std::fs::read(&test.cert_path).unwrap();
        for cert in rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        {
            root_store.add(cert).unwrap();
        }
        let client_config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&ssl_request_packet()).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), b'S');

        let mut tls_stream = connector.connect("localhost".try_into().unwrap(), stream).await.unwrap();
        tls_stream.write_all(&startup_packet()).await.unwrap();

        assert_eq!(tls_stream.read_u8().await.unwrap(), b'R');
        let len = tls_stream.read_i32().await.unwrap();
        assert_eq!(len, 8);
        assert_eq!(tls_stream.read_i32().await.unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ssl_request_without_cert_responds_n() {
        let auth = Arc::new(AuthConfig::default());
        let tls = TlsSettings::default();
        let addr = spawn_test_server(auth, tls, None).await;

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&ssl_request_packet()).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), b'N');
        stream.write_all(&startup_packet()).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), b'R');

        let _ = stream.read_i32().await.unwrap();
        assert_eq!(stream.read_i32().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn require_tls_rejects_plaintext_startup() {
        let auth = Arc::new(AuthConfig::default());
        let tls = TlsSettings {
            cert_path: None,
            key_path: None,
            require_tls: true,
        };
        let addr = spawn_test_server(auth, tls, None).await;

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&startup_packet()).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), b'E');
    }

    #[tokio::test]
    async fn password_requires_tls_when_configured() {
        let dir = std::env::temp_dir().join(format!("moodeng_tls_pw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let test = generate_test_tls(&dir);
        let acceptor = Arc::new(TlsAcceptor::from(test.server_config.clone()));

        let auth = Arc::new(AuthConfig {
            password_hash: Some(hash_password("secret")),
            scram_credentials: None,
            require_tls_for_password: true,
        });
        let addr = spawn_test_server(auth, test.settings.clone(), Some(acceptor)).await;

        // Plaintext startup with password should fail before auth completes
        let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
        plain.write_all(&startup_packet()).await.unwrap();
        assert_eq!(plain.read_u8().await.unwrap(), b'E');

        // TLS path should accept password
        let mut root_store = RootCertStore::empty();
        let cert_pem = std::fs::read(&test.cert_path).unwrap();
        for cert in rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        {
            root_store.add(cert).unwrap();
        }
        let client_config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&ssl_request_packet()).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), b'S');
        let mut tls_stream = connector.connect("localhost".try_into().unwrap(), stream).await.unwrap();
        tls_stream.write_all(&startup_packet()).await.unwrap();
        assert_eq!(tls_stream.read_u8().await.unwrap(), b'R');
        assert_eq!(tls_stream.read_i32().await.unwrap(), 8);
        assert_eq!(tls_stream.read_i32().await.unwrap(), 3);
        tls_stream.write_u8(b'p').await.unwrap();
        tls_stream
            .write_i32((b"secret\0".len() + 4) as i32)
            .await
            .unwrap();
        tls_stream.write_all(b"secret\0").await.unwrap();
        assert_eq!(tls_stream.read_u8().await.unwrap(), b'R');
        assert_eq!(tls_stream.read_i32().await.unwrap(), 8);
        assert_eq!(tls_stream.read_i32().await.unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scram_sha256_handshake_succeeds() {
        use crate::scram::ScramCredentials;

        let auth = Arc::new(AuthConfig {
            password_hash: None,
            scram_credentials: Some(ScramCredentials::from_password("secret")),
            require_tls_for_password: false,
        });
        let addr = spawn_test_server(auth, TlsSettings::default(), None).await;

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&startup_packet()).await.unwrap();

        assert_eq!(stream.read_u8().await.unwrap(), b'R');
        let init_len = stream.read_i32().await.unwrap() as usize;
        let mut init_body = vec![0u8; init_len.saturating_sub(4)];
        stream.read_exact(&mut init_body).await.unwrap();
        assert_eq!(i32::from_be_bytes(init_body[..4].try_into().unwrap()), 10);

        let client_first = b"n,,n=moodeng,r=clientnonce123";
        let mut sasl = Vec::new();
        sasl.extend_from_slice(b"SCRAM-SHA-256\0");
        sasl.extend_from_slice(&(client_first.len() as i32).to_be_bytes());
        sasl.extend_from_slice(client_first);
        stream.write_u8(b'p').await.unwrap();
        stream.write_i32((sasl.len() + 4) as i32).await.unwrap();
        stream.write_all(&sasl).await.unwrap();

        assert_eq!(stream.read_u8().await.unwrap(), b'R');
        let msg_len = stream.read_i32().await.unwrap() as usize;
        let mut body = vec![0u8; msg_len.saturating_sub(4)];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(i32::from_be_bytes(body[..4].try_into().unwrap()), 11);
        let server_first = String::from_utf8_lossy(&body[4..])
            .trim_end_matches('\0')
            .to_string();

        let client_final = build_scram_client_final("secret", client_first, &server_first);
        let mut sasl2 = Vec::new();
        sasl2.push(0);
        sasl2.extend_from_slice(&(client_final.len() as i32).to_be_bytes());
        sasl2.extend_from_slice(client_final.as_bytes());
        stream.write_u8(b'p').await.unwrap();
        stream.write_i32((sasl2.len() + 4) as i32).await.unwrap();
        stream.write_all(&sasl2).await.unwrap();

        assert_eq!(stream.read_u8().await.unwrap(), b'R');
        let fin_len = stream.read_i32().await.unwrap() as usize;
        let mut fin_body = vec![0u8; fin_len.saturating_sub(4)];
        stream.read_exact(&mut fin_body).await.unwrap();
        assert_eq!(i32::from_be_bytes(fin_body[..4].try_into().unwrap()), 12);

        assert_eq!(stream.read_u8().await.unwrap(), b'R');
        assert_eq!(stream.read_i32().await.unwrap(), 8);
        assert_eq!(stream.read_i32().await.unwrap(), 0);
    }

    fn build_scram_client_final(password: &str, client_first: &[u8], server_first: &str) -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        let client_first_str = std::str::from_utf8(client_first).unwrap();
        let bare = client_first_str
            .strip_prefix("n,,")
            .expect("client-first gs2 header");
        let mut server_attrs = std::collections::HashMap::new();
        for part in server_first.split(',') {
            let (k, v) = part.split_once('=').unwrap();
            server_attrs.insert(k.to_string(), v.to_string());
        }
        let salt = STANDARD.decode(server_attrs["s"].as_str()).unwrap();
        let iterations: u32 = server_attrs["i"].parse().unwrap();
        let combined_nonce = server_attrs["r"].clone();

        fn hi(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;
            let mut ui = {
                let mut mac = HmacSha256::new_from_slice(salt).unwrap();
                mac.update(password.as_bytes());
                let mut out = [0u8; 32];
                out.copy_from_slice(&mac.finalize().into_bytes());
                out
            };
            let mut result = ui;
            for _ in 1..iterations {
                let mut mac = HmacSha256::new_from_slice(&ui).unwrap();
                mac.update(password.as_bytes());
                ui = {
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&mac.finalize().into_bytes());
                    out
                };
                for (acc, val) in result.iter_mut().zip(ui.iter()) {
                    *acc ^= val;
                }
            }
            result
        }

        fn hmac_sha256(key: &[u8], data: &str) -> [u8; 32] {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
            mac.update(data.as_bytes());
            let mut out = [0u8; 32];
            out.copy_from_slice(&mac.finalize().into_bytes());
            out
        }

        fn sha256(data: &[u8]) -> [u8; 32] {
            use sha2::{Digest, Sha256};
            let mut out = [0u8; 32];
            out.copy_from_slice(&Sha256::digest(data));
            out
        }

        let salted_password = hi(password, &salt, iterations);
        let client_key = hmac_sha256(&salted_password, "Client Key");
        let stored_key = sha256(&client_key);
        let without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message = format!("{bare},{server_first},{without_proof}");
        let client_signature = hmac_sha256(&stored_key, &auth_message);
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        format!("{without_proof},p={}", STANDARD.encode(proof))
    }
}
