use clap::Parser;
use std::io::{Read, Write};
use std::net::TcpStream;

#[derive(Parser, Debug)]
#[command(name = "moodeng", about = "MoodengSQL interactive client")]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1")]
    host: String,

    #[arg(short, long, default_value_t = 5432)]
    port: u16,

    #[arg(short, long)]
    command: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = format!("{}:{}", args.host, args.port);

    if let Some(cmd) = args.command {
        let result = execute_sql(&addr, &cmd)?;
        print!("{result}");
        return Ok(());
    }

    println!("MoodengSQL Client — connected to {addr}");
    println!("Type SQL queries, \\q to quit.\n");

    let mut editor = rustyline::DefaultEditor::new()?;
    loop {
        let line = editor.readline("moodeng> ")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "\\q" || trimmed.eq_ignore_ascii_case("quit") {
            break;
        }
        match execute_sql(&addr, trimmed) {
            Ok(out) => print!("{out}"),
            Err(e) => eprintln!("ERROR: {e}"),
        }
    }

    Ok(())
}

fn execute_sql(addr: &str, sql: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect(addr)?;

    // Startup message (PostgreSQL protocol 3.0)
    let mut payload = Vec::new();
    payload.extend_from_slice(&196608i32.to_be_bytes());
    payload.extend_from_slice(b"user");
    payload.push(0);
    payload.extend_from_slice(b"moodeng");
    payload.push(0);
    payload.push(0);
    let mut startup = Vec::new();
    startup.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&payload);
    stream.write_all(&startup)?;

    // Read until ReadyForQuery (startup handshake)
    read_until_ready(&mut stream)?;

    // Send simple query
    let mut query_msg = vec![b'Q'];
    let sql_bytes = sql.as_bytes();
    let qlen = (sql_bytes.len() + 5) as i32;
    query_msg.extend_from_slice(&qlen.to_be_bytes());
    query_msg.extend_from_slice(sql_bytes);
    query_msg.push(0);
    stream.write_all(&query_msg)?;

    // Read until ReadyForQuery (query response)
    let mut output = String::new();
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header)?;
        let msg_type = header[0];
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; len.saturating_sub(4)];
        if !payload.is_empty() {
            stream.read_exact(&mut payload)?;
        }

        match msg_type {
            b'Z' => break,
            b'E' => {
                output.push_str("ERROR: ");
                if let Some(pos) = payload.iter().position(|&b| b == 0) {
                    output.push_str(&String::from_utf8_lossy(&payload[1..pos]));
                }
                output.push('\n');
            }
            b'T' => {
                // RowDescription — parse column names
                if payload.len() > 2 {
                    let ncols = i16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut pos = 2;
                    let mut cols = Vec::new();
                    for _ in 0..ncols {
                        if let Some(end) = payload[pos..].iter().position(|&b| b == 0) {
                            cols.push(String::from_utf8_lossy(&payload[pos..pos + end]).to_string());
                            pos += end + 1 + 18;
                        }
                    }
                    output.push_str(&cols.join(" | "));
                    output.push('\n');
                    output.push_str(&"-".repeat(cols.join(" | ").len()));
                    output.push('\n');
                }
            }
            b'D' => {
                if payload.len() > 2 {
                    let ncols = i16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut pos = 2;
                    let mut vals = Vec::new();
                    for _ in 0..ncols {
                        if pos + 4 > payload.len() {
                            break;
                        }
                        let vlen = i32::from_be_bytes([
                            payload[pos],
                            payload[pos + 1],
                            payload[pos + 2],
                            payload[pos + 3],
                        ]);
                        pos += 4;
                        if vlen == -1 {
                            vals.push("NULL".to_string());
                        } else {
                            vals.push(
                                String::from_utf8_lossy(&payload[pos..pos + vlen as usize])
                                    .to_string(),
                            );
                            pos += vlen as usize;
                        }
                    }
                    output.push_str(&vals.join(" | "));
                    output.push('\n');
                }
            }
            b'C' => {
                if let Some(pos) = payload.iter().position(|&b| b == 0) {
                    output.push_str(&String::from_utf8_lossy(&payload[..pos]));
                    output.push('\n');
                }
            }
            _ => {}
        }
    }

    // Send terminate
    stream.write_all(&[b'X', 0, 0, 0, 4])?;
    Ok(output)
}

fn read_until_ready(stream: &mut TcpStream) -> anyhow::Result<()> {
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header)?;
        let msg_type = header[0];
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut payload = vec![0u8; len.saturating_sub(4)];
        if !payload.is_empty() {
            stream.read_exact(&mut payload)?;
        }
        if msg_type == b'Z' {
            break;
        }
    }
    Ok(())
}
