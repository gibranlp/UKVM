//! Development node identity. The public certificate is exchanged manually;
//! the private key never leaves this directory.

use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Identity {
    pub id: String,
    pub cert_path: PathBuf,
    cert: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    pub fn load_or_create(directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let cert_path = directory.join("node.der");
        let key_path = directory.join("node.key");
        if !cert_path.exists() && !key_path.exists() {
            let generated = generate_simple_self_signed(vec!["ukvm-node".to_string()])
                .map_err(|e| format!("certificate generation failed: {e}"))?;
            let key = generated.key_pair.serialize_der();
            let cert = generated.cert.der().to_vec();
            write_new_private(&key_path, &key).map_err(|e| e.to_string())?;
            fs::write(&cert_path, cert).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            let metadata = fs::symlink_metadata(&key_path).map_err(|e| e.to_string())?;
            if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err("private key must be a regular file readable only by its owner".into());
            }
        }
        let cert = fs::read(&cert_path).map_err(|e| format!("public certificate: {e}"))?;
        let key = fs::read(&key_path).map_err(|e| format!("private key: {e}"))?;
        if cert.len() > 4096 || key.len() > 4096 || cert.is_empty() || key.is_empty() {
            return Err("identity certificate or key has invalid length".into());
        }
        Ok(Self {
            id: certificate_id(&cert),
            cert_path,
            cert,
            key,
        })
    }

    pub fn tls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert.clone())
    }
    pub fn tls_key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
}

pub fn certificate_id(cert: &[u8]) -> String {
    let hash = Sha256::digest(cert);
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn default_state_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|path| path.join("ukvm"))
        .ok_or("unable to locate user data directory".into())
}

pub fn load_peer_certificate(path: &Path) -> Result<CertificateDer<'static>, String> {
    let cert = fs::read(path).map_err(|e| format!("peer certificate: {e}"))?;
    if cert.is_empty() || cert.len() > 4096 {
        return Err("peer certificate length is invalid".into());
    }
    Ok(CertificateDer::from(cert))
}

fn write_new_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub fn client_config(
    identity: &Identity,
    peer: CertificateDer<'static>,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(peer)
        .map_err(|e| format!("invalid peer certificate: {e}"))?;
    let config = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![identity.tls_cert()], identity.tls_key())
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

pub fn server_config(
    identity: &Identity,
    peer: CertificateDer<'static>,
) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(peer)
        .map_err(|e| format!("invalid peer certificate: {e}"))?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| e.to_string())?;
    let config = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.tls_cert()], identity.tls_key())
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_stable_and_private() {
        let dir = std::env::temp_dir().join(format!("ukvm-identity-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_eq!(first.id, second.id);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(dir.join("node.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
