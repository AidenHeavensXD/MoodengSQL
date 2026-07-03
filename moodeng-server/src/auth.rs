use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::rngs::OsRng;

use crate::scram::{generate_scram_secret, ScramCredentials};

/// Resolved authentication settings for the server.
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// Argon2 password hash for cleartext-password fallback (auth type 3).
    pub password_hash: Option<String>,
    /// SCRAM-SHA-256 credentials for SASL authentication (auth type 10).
    pub scram_credentials: Option<ScramCredentials>,
    /// When true, cleartext password messages are rejected unless the connection uses TLS.
    pub require_tls_for_password: bool,
}

impl AuthConfig {
    pub fn from_config_and_env(
        hash_from_file: Option<String>,
        scram_from_file: Option<String>,
    ) -> Self {
        let password_hash = hash_from_file.filter(|h| !h.is_empty());
        let scram_credentials = scram_from_file
            .filter(|s| !s.is_empty())
            .and_then(|s| ScramCredentials::parse(&s).ok());

        if password_hash.is_some() || scram_credentials.is_some() {
            return Self {
                password_hash,
                scram_credentials,
                require_tls_for_password: false,
            };
        }

        if let Ok(password) = std::env::var("MOODENG_PASSWORD") {
            if !password.is_empty() {
                return Self {
                    password_hash: Some(hash_password(&password)),
                    scram_credentials: Some(ScramCredentials::from_password(&password)),
                    require_tls_for_password: false,
                };
            }
        }
        Self::default()
    }

    pub fn required(&self) -> bool {
        self.password_hash.is_some() || self.scram_credentials.is_some()
    }

    pub fn scram_available(&self) -> bool {
        self.scram_credentials.is_some()
    }

    pub fn verify(&self, password: &str) -> bool {
        match &self.password_hash {
            Some(hash) => verify_password(password, hash),
            None => true,
        }
    }
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn hash_scram(password: &str) -> String {
    generate_scram_secret(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_roundtrip() {
        let hash = hash_password("moodeng-secret");
        assert!(verify_password("moodeng-secret", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn env_password_generates_scram_and_argon2() {
        std::env::set_var("MOODENG_PASSWORD", "env-secret");
        let auth = AuthConfig::from_config_and_env(None, None);
        std::env::remove_var("MOODENG_PASSWORD");
        assert!(auth.scram_available());
        assert!(auth.password_hash.is_some());
    }

    #[tokio::test]
    async fn cleartext_password_handshake() {
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let auth = Arc::new(AuthConfig {
            password_hash: Some(hash_password("secret")),
            scram_credentials: None,
            require_tls_for_password: false,
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let len = stream.read_i32().await.unwrap() as usize;
            let mut buf = vec![0u8; len.saturating_sub(4)];
            stream.read_exact(&mut buf).await.unwrap();

            stream.write_u8(b'R').await.unwrap();
            stream.write_i32(8).await.unwrap();
            stream.write_i32(3).await.unwrap();

            assert_eq!(stream.read_u8().await.unwrap(), b'p');
            let plen = stream.read_i32().await.unwrap() as usize;
            let mut pw = vec![0u8; plen.saturating_sub(4)];
            stream.read_exact(&mut pw).await.unwrap();
            let password = String::from_utf8_lossy(&pw)
                .trim_end_matches('\0')
                .to_string();
            assert!(auth.verify(&password));
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut body = Vec::new();
        body.extend_from_slice(&196608i32.to_be_bytes());
        body.extend_from_slice(b"user\0moodeng\0");
        let mut startup = Vec::new();
        startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        startup.extend_from_slice(&body);
        client.write_all(&startup).await.unwrap();

        client.read_u8().await.unwrap();
        client.read_i32().await.unwrap();
        assert_eq!(client.read_i32().await.unwrap(), 3);

        let pw_msg = b"secret\0".to_vec();
        client.write_u8(b'p').await.unwrap();
        client.write_i32((pw_msg.len() + 4) as i32).await.unwrap();
        client.write_all(&pw_msg).await.unwrap();

        server.await.unwrap();
    }
}
