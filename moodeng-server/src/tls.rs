use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig as RustlsServerConfig;
use tokio_rustls::rustls;

use crate::config::ServerConfig;

/// TLS settings loaded from server config.
#[derive(Debug, Clone, Default)]
pub struct TlsSettings {
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    pub require_tls: bool,
}

impl TlsSettings {
    pub fn from_server_config(cfg: &ServerConfig) -> Self {
        Self {
            cert_path: cfg.tls_cert.clone(),
            key_path: cfg.tls_key.clone(),
            require_tls: cfg.require_tls,
        }
    }

    pub fn tls_available(&self) -> bool {
        self.cert_path.as_ref().is_some() && self.key_path.as_ref().is_some()
    }

    pub fn load_server_config(&self) -> anyhow::Result<Option<Arc<RustlsServerConfig>>> {
        let (Some(cert_path), Some(key_path)) = (&self.cert_path, &self.key_path) else {
            if self.require_tls {
                anyhow::bail!("require_tls is enabled but tls_cert / tls_key are not configured");
            }
            return Ok(None);
        };

        let certs = load_certs(cert_path)?;
        let key = load_private_key(key_path)?;
        let config = RustlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        Ok(Some(Arc::new(config)))
    }
}

fn load_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("read cert {path:?}: {e}"))
}

fn load_private_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("read key {path:?}: {e}"))?;
    keys.into_iter()
        .next()
        .map(PrivateKeyDer::Pkcs8)
        .ok_or_else(|| anyhow::anyhow!("no private key found in {path:?}"))
}

#[cfg(test)]
pub mod test_util {
    use super::*;
    use rcgen::{CertificateParams, KeyPair, SanType};
    use std::io::Write;

    pub struct TestCert {
        pub cert_path: PathBuf,
        pub key_path: PathBuf,
        pub settings: TlsSettings,
        pub server_config: Arc<RustlsServerConfig>,
    }

    pub fn generate_test_tls(dir: &Path) -> TestCert {
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params
            .subject_alt_names
            .push(SanType::IpAddress(std::net::IpAddr::V4(
                std::net::Ipv4Addr::LOCALHOST,
            )));
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let cert_path = dir.join("test-cert.pem");
        let key_path = dir.join("test-key.pem");
        let mut cert_file = File::create(&cert_path).unwrap();
        cert_file
            .write_all(cert.pem().as_bytes())
            .unwrap();
        let mut key_file = File::create(&key_path).unwrap();
        key_file
            .write_all(key_pair.serialize_pem().as_bytes())
            .unwrap();

        let settings = TlsSettings {
            cert_path: Some(cert_path.clone()),
            key_path: Some(key_path.clone()),
            require_tls: false,
        };
        let server_config = settings.load_server_config().unwrap().unwrap();
        TestCert {
            cert_path,
            key_path,
            settings,
            server_config,
        }
    }
}
