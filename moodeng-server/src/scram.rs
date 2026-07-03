use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const SCRAM_SHA256: &str = "SCRAM-SHA-256";
pub const DEFAULT_ITERATIONS: u32 = 4096;

#[derive(Debug, Clone)]
pub struct ScramCredentials {
    pub iterations: u32,
    pub salt: Vec<u8>,
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

#[derive(Debug)]
pub struct ScramSession {
    creds: ScramCredentials,
    client_first_bare: String,
    server_first: String,
    client_nonce: String,
}

impl ScramCredentials {
    pub fn from_password(password: &str) -> Self {
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self::from_password_with_salt(password, &salt, DEFAULT_ITERATIONS)
    }

    pub fn from_password_with_salt(password: &str, salt: &[u8], iterations: u32) -> Self {
        let salted_password = hi(password, salt, iterations);
        let client_key = hmac_sha256(&salted_password, "Client Key");
        let stored_key = sha256(&client_key);
        let server_key = hmac_sha256(&salted_password, "Server Key");
        Self {
            iterations,
            salt: salt.to_vec(),
            stored_key,
            server_key,
        }
    }

    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let rest = raw
            .strip_prefix("SCRAM-SHA-256$")
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM credential prefix"))?;
        let (params, keys) = rest
            .split_once('$')
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM credential format"))?;
        let (iterations, salt_b64) = params
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM params"))?;
        let (stored_b64, server_b64) = keys
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM keys"))?;
        Ok(Self {
            iterations: iterations.parse()?,
            salt: STANDARD.decode(salt_b64)?,
            stored_key: decode_32(stored_b64)?,
            server_key: decode_32(server_b64)?,
        })
    }

    pub fn to_secret_string(&self) -> String {
        format!(
            "SCRAM-SHA-256${}:{}${}:{}",
            self.iterations,
            STANDARD.encode(&self.salt),
            STANDARD.encode(self.stored_key),
            STANDARD.encode(self.server_key)
        )
    }
}

impl ScramSession {
    pub fn start(creds: ScramCredentials, client_first: &str) -> anyhow::Result<(Self, String)> {
        let client_first_bare = strip_gs2_header(client_first)?;
        let mut attrs = parse_attrs(&client_first_bare)?;
        let client_nonce = attrs
            .remove("r")
            .ok_or_else(|| anyhow::anyhow!("SCRAM client-first missing nonce"))?;
        if client_nonce.is_empty() {
            anyhow::bail!("SCRAM client nonce must not be empty");
        }

        let mut server_nonce_bytes = [0u8; 18];
        rand::rngs::OsRng.fill_bytes(&mut server_nonce_bytes);
        let server_nonce = STANDARD.encode(server_nonce_bytes);
        let combined_nonce = format!("{client_nonce}{server_nonce}");
        let server_first = format!(
            "r={combined_nonce},s={},i={}",
            STANDARD.encode(&creds.salt),
            creds.iterations
        );
        Ok((
            Self {
                creds,
                client_first_bare,
                server_first: server_first.clone(),
                client_nonce,
            },
            server_first,
        ))
    }

    pub fn finish(&self, client_final: &str) -> anyhow::Result<String> {
        let attrs = parse_attrs(client_final)?;
        let channel_binding = attrs
            .get("c")
            .ok_or_else(|| anyhow::anyhow!("SCRAM client-final missing channel binding"))?;
        if channel_binding != "biws" && channel_binding != "eSws" {
            anyhow::bail!("unsupported SCRAM channel binding: {channel_binding}");
        }
        let combined_nonce = attrs
            .get("r")
            .ok_or_else(|| anyhow::anyhow!("SCRAM client-final missing nonce"))?;
        if !combined_nonce.starts_with(&self.client_nonce) {
            anyhow::bail!("SCRAM nonce mismatch");
        }
        let proof_b64 = attrs
            .get("p")
            .ok_or_else(|| anyhow::anyhow!("SCRAM client-final missing proof"))?;
        let client_proof = STANDARD.decode(proof_b64)?;

        let without_proof = client_final
            .split(",p=")
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM client-final"))?;
        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, self.server_first, without_proof
        );

        let client_signature = hmac_sha256(&self.creds.stored_key, &auth_message);
        let client_key = xor_bytes(&client_proof, &client_signature);
        if sha256(&client_key) != self.creds.stored_key {
            anyhow::bail!("SCRAM proof verification failed");
        }

        let server_signature = hmac_sha256(&self.creds.server_key, &auth_message);
        Ok(format!("v={}", STANDARD.encode(server_signature)))
    }
}

pub fn generate_scram_secret(password: &str) -> String {
    ScramCredentials::from_password(password).to_secret_string()
}

fn decode_32(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = STANDARD.decode(value)?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32-byte SCRAM key");
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn strip_gs2_header(client_first: &str) -> anyhow::Result<String> {
    if let Some(rest) = client_first.strip_prefix("n,,") {
        return Ok(rest.to_string());
    }
    if let Some(rest) = client_first.strip_prefix("y,,") {
        return Ok(rest.to_string());
    }
    anyhow::bail!("unsupported SCRAM gs2 header");
}

fn parse_attrs(input: &str) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    for part in input.split(',') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid SCRAM attribute: {part}"))?;
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

fn hi(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut ui = hmac_sha256_bytes(salt, password.as_bytes());
    let mut result = ui;
    for _ in 1..iterations {
        ui = hmac_sha256_bytes(&ui, password.as_bytes());
        for (acc, val) in result.iter_mut().zip(ui.iter()) {
            *acc ^= val;
        }
    }
    result
}

fn hmac_sha256(key: &[u8], data: &str) -> [u8; 32] {
    hmac_sha256_bytes(key, data.as_bytes())
}

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scram_roundtrip_generate_and_parse() {
        let secret = generate_scram_secret("moodeng-secret");
        let creds = ScramCredentials::parse(&secret).unwrap();
        assert_eq!(creds.iterations, DEFAULT_ITERATIONS);
        assert_eq!(creds.salt.len(), 16);
    }

    #[test]
    fn scram_server_client_exchange() {
        let password = "pencil";
        let creds = ScramCredentials::from_password_with_salt(
            password,
            &STANDARD.decode("qsXUXOU4aDCV4I/4MXIU0l1TaAOmC5mQ5N/V9YcF3mU=").unwrap(),
            4096,
        );
        let client_first = "n,,n=user,r=fyko+dJokKKxb5l1jPgXg==";
        let (session, server_first) = ScramSession::start(creds, client_first).unwrap();
        assert!(server_first.contains("r=fyko+dJokKKxb5l1jPgXg=="));
        let client_final = build_client_final(password, client_first, &server_first);
        let server_final = session.finish(&client_final).unwrap();
        assert!(server_final.starts_with("v="));
    }

    fn build_client_final(password: &str, client_first: &str, server_first: &str) -> String {
        let bare = strip_gs2_header(client_first).unwrap();
        let server_attrs = parse_attrs(server_first).unwrap();
        let salt = STANDARD.decode(server_attrs["s"].as_str()).unwrap();
        let iterations: u32 = server_attrs["i"].parse().unwrap();
        let combined_nonce = server_attrs["r"].clone();

        let salted_password = hi(password, &salt, iterations);
        let client_key = hmac_sha256(&salted_password, "Client Key");
        let stored_key = sha256(&client_key);
        let without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message = format!("{bare},{server_first},{without_proof}");
        let client_signature = hmac_sha256(&stored_key, &auth_message);
        let proof = xor_bytes(&client_key, &client_signature);
        format!("{without_proof},p={}", STANDARD.encode(proof))
    }
}
