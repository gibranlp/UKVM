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

#[derive(Clone)]
pub struct Identity {
    pub id: String,
    pub cert_path: PathBuf,
    cert: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    pub fn load_or_create(directory: &Path) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            Self::load_windows(directory)
        }
        #[cfg(not(target_os = "windows"))]
        {
            crate::storage::directory(directory).map_err(|e| e.to_string())?;
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
                    return Err(
                        "private key must be a regular file readable only by its owner".into(),
                    );
                }
            }
            let cert = crate::storage::read(&cert_path, 4096)
                .map_err(|e| format!("public certificate: {e}"))?;
            let key =
                crate::storage::read(&key_path, 4096).map_err(|e| format!("private key: {e}"))?;
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
    }

    #[cfg(target_os = "windows")]
    fn load_windows(directory: &Path) -> Result<Self, String> {
        use std::ffi::OsStr;
        let dir = ukvm_platform::windows::state::StateDirectory::open(directory, true)
            .map_err(|e| e.to_string())?;
        let key = dir.read(OsStr::new("node.key"), 4096);
        let cert = dir.read(OsStr::new("node.der"), 4096);
        let (cert, key) = match (cert, key) {
            (Err(cert), Err(key)) if cert.kind() == io::ErrorKind::NotFound && key.kind() == io::ErrorKind::NotFound => {
                let generated = generate_simple_self_signed(vec!["ukvm-node".to_string()]).map_err(|_| "certificate generation failed")?;
                let cert = generated.cert.der().to_vec(); let key = generated.key_pair.serialize_der();
                dir.write_new(OsStr::new("node.key"), &key).map_err(|e| e.to_string())?;
                dir.write_new(OsStr::new("node.der"), &cert).map_err(|e| e.to_string())?;
                (cert, key)
            }
            (Ok(cert), Ok(key)) => (cert, key),
            _ => return Err("identity is incomplete or its private storage is unavailable; no key or certificate was overwritten".into()),
        };
        if cert.is_empty() || key.is_empty() {
            return Err("identity certificate or key has invalid length".into());
        }
        Ok(Self {
            id: certificate_id(&cert),
            cert_path: directory.join("node.der"),
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

impl std::fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Identity")
            .field("id", &self.id)
            .field("cert_path", &self.cert_path)
            .field("key", &"<redacted>")
            .finish_non_exhaustive()
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

#[cfg(not(target_os = "windows"))]
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
    server_config_many(identity, vec![peer])
}

pub fn server_config_many(
    identity: &Identity,
    peers: Vec<CertificateDer<'static>>,
) -> Result<Arc<rustls::ServerConfig>, String> {
    if peers.is_empty() {
        return Err("no trusted peer certificates configured".into());
    }
    let mut roots = rustls::RootCertStore::empty();
    for peer in peers {
        roots
            .add(peer)
            .map_err(|e| format!("invalid peer certificate: {e}"))?;
    }
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
    fn identity_debug_never_includes_certificate_or_private_key_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let rendered = format!("{identity:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("key: ["));
        assert!(!rendered.contains("cert: ["));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn unsafe_and_incomplete_windows_identity_state_is_not_overwritten() {
        use std::ffi::OsStr;
        let temp = tempfile::tempdir().unwrap();
        let unsafe_dir = temp.path().join("unsafe");
        fs::create_dir(&unsafe_dir).unwrap();
        assert!(Identity::load_or_create(&unsafe_dir).is_err());
        assert!(!unsafe_dir.join("node.key").exists());
        let path = temp.path().join("private");
        let directory = ukvm_platform::windows::state::StateDirectory::open(&path, true).unwrap();
        directory
            .write_new(OsStr::new("node.key"), b"incomplete fixture")
            .unwrap();
        assert!(Identity::load_or_create(&path).is_err());
        assert_eq!(
            directory.read(OsStr::new("node.key"), 4096).unwrap(),
            b"incomplete fixture"
        );
        assert!(!path.join("node.der").exists());
    }
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
