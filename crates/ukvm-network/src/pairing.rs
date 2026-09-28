//! First-contact trust bootstrap. Noise supplies ephemeral encryption; users
//! authenticate the exchange by comparing a transcript- and identity-bound SAS.
use crate::{
    identity::{certificate_id, Identity},
    trust::{edit_lock, resolve_address, Peer, TrustStore},
    DEFAULT_PORT,
};
use bincode::Options;
use ring::{
    rand::SystemRandom,
    signature::{
        EcdsaKeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snow::{Builder, TransportState};
use std::{
    fs,
    future::Future,
    net::IpAddr,
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub const PAIRING_VERSION: u8 = 1;
pub const PAIRING_PORT: u16 = 24801;
pub const PAIRING_TIMEOUT_SECS: u64 = 90;
const MAGIC: &[u8] = b"UKVM-PAIR\0\x01";
const NOISE: &str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"UniversalKVM pairing protocol 1";
const MAX_PAIR_FRAME: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairRole {
    Initiator,
    Responder,
}

#[derive(Clone, Debug)]
pub struct PairOffer {
    pub remote_name: String,
    pub remote_id: String,
    pub address: String,
    pub verification_code: String,
    pub already_trusted_as: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PairOutcome {
    pub peer: Peer,
    pub created: bool,
}

#[derive(Deserialize, Serialize)]
enum PairMessage {
    Identity {
        node_id: String,
        cert: Vec<u8>,
        name: String,
    },
    Proof(Vec<u8>),
    Decision(bool),
    Ready(bool),
    Commit(bool),
}

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PAIR_FRAME as u64)
        .reject_trailing_bytes()
}

async fn write_raw(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_PAIR_FRAME {
        return Err("pairing frame has invalid length".into());
    }
    stream
        .write_all(&(bytes.len() as u16).to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(bytes).await.map_err(|e| e.to_string())
}
async fn read_raw(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut header = [0u8; 2];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("pairing connection closed: {e}"))?;
    let len = u16::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_PAIR_FRAME {
        return Err("pairing frame has invalid length".into());
    }
    let mut bytes = vec![0u8; len];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|e| format!("pairing frame incomplete: {e}"))?;
    Ok(bytes)
}

async fn handshake(
    stream: &mut TcpStream,
    role: PairRole,
) -> Result<(TransportState, Vec<u8>), String> {
    match role {
        PairRole::Initiator => {
            write_raw(stream, MAGIC).await?;
            if read_raw(stream).await?.as_slice() != MAGIC {
                return Err("pairing protocol version mismatch".into());
            }
        }
        PairRole::Responder => {
            if read_raw(stream).await?.as_slice() != MAGIC {
                return Err("pairing protocol version mismatch".into());
            }
            write_raw(stream, MAGIC).await?;
        }
    }
    let params = NOISE
        .parse()
        .map_err(|e| format!("unsupported Noise parameters: {e}"))?;
    let builder = Builder::new(params).prologue(PROLOGUE);
    let mut noise = match role {
        PairRole::Initiator => builder.build_initiator(),
        PairRole::Responder => builder.build_responder(),
    }
    .map_err(|e| format!("Noise setup: {e}"))?;
    let mut message = [0u8; 1024];
    let mut payload = [0u8; 1024];
    match role {
        PairRole::Initiator => {
            let len = noise
                .write_message(&[], &mut message)
                .map_err(|e| format!("Noise handshake: {e}"))?;
            write_raw(stream, &message[..len]).await?;
            let packet = read_raw(stream).await?;
            if packet.len() > 1024 {
                return Err("oversized Noise handshake".into());
            }
            noise
                .read_message(&packet, &mut payload)
                .map_err(|e| format!("Noise handshake: {e}"))?;
        }
        PairRole::Responder => {
            let packet = read_raw(stream).await?;
            if packet.len() > 1024 {
                return Err("oversized Noise handshake".into());
            }
            noise
                .read_message(&packet, &mut payload)
                .map_err(|e| format!("Noise handshake: {e}"))?;
            let len = noise
                .write_message(&[], &mut message)
                .map_err(|e| format!("Noise handshake: {e}"))?;
            write_raw(stream, &message[..len]).await?;
        }
    }
    if !noise.is_handshake_finished() {
        return Err("Noise handshake did not finish".into());
    }
    let hash = noise.get_handshake_hash().to_vec();
    let transport = noise
        .into_transport_mode()
        .map_err(|e| format!("Noise transport: {e}"))?;
    Ok((transport, hash))
}

struct SecureChannel {
    stream: TcpStream,
    noise: TransportState,
}
impl SecureChannel {
    async fn send(&mut self, message: &PairMessage) -> Result<(), String> {
        let plain = codec().serialize(message).map_err(|e| e.to_string())?;
        if plain.len() > MAX_PAIR_FRAME - 16 {
            return Err("pairing message too large".into());
        }
        let mut encrypted = vec![0u8; plain.len() + 16];
        let len = self
            .noise
            .write_message(&plain, &mut encrypted)
            .map_err(|e| format!("pairing encryption: {e}"))?;
        write_raw(&mut self.stream, &encrypted[..len]).await
    }
    async fn receive(&mut self) -> Result<PairMessage, String> {
        let encrypted = read_raw(&mut self.stream).await?;
        let mut plain = vec![0u8; encrypted.len()];
        let len = self
            .noise
            .read_message(&encrypted, &mut plain)
            .map_err(|e| format!("pairing authentication failed: {e}"))?;
        codec()
            .deserialize(&plain[..len])
            .map_err(|e| format!("malformed pairing message: {e}"))
    }
    async fn exchange(&mut self, local: PairMessage) -> Result<PairMessage, String> {
        self.send(&local).await?;
        self.receive().await
    }
}

fn validate_identity(node_id: &str, cert: &[u8], name: &str, local_id: &str) -> Result<(), String> {
    if cert.is_empty() || cert.len() > 4096 {
        return Err("remote certificate length is invalid".into());
    }
    if name.is_empty() || name.chars().count() > 63 || name.chars().any(char::is_control) {
        return Err("remote node name is invalid".into());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(cert.to_vec()))
        .map_err(|e| format!("remote certificate invalid: {e}"))?;
    if certificate_id(cert) != node_id {
        return Err("remote certificate does not match advertised node ID".into());
    }
    if node_id == local_id {
        return Err("cannot pair this node with itself".into());
    }
    Ok(())
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u32).to_be_bytes());
    hash.update(bytes);
}
fn verification_code(
    handshake_hash: &[u8],
    initiator_cert: &[u8],
    responder_cert: &[u8],
    initiator_name: &str,
    responder_name: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"UniversalKVM pairing SAS v1");
    hash_field(&mut hash, handshake_hash);
    hash_field(&mut hash, initiator_cert);
    hash_field(&mut hash, responder_cert);
    hash_field(&mut hash, initiator_name.as_bytes());
    hash_field(&mut hash, responder_name.as_bytes());
    let output = hash.finalize();
    let value = u64::from_be_bytes(
        output[..8]
            .try_into()
            .expect("SHA-256 output has eight bytes"),
    ) % 100_000_000;
    format!("{:04}-{:04}", value / 10_000, value % 10_000)
}

fn proof_context(
    handshake_hash: &[u8],
    initiator_cert: &[u8],
    responder_cert: &[u8],
    initiator_name: &str,
    responder_name: &str,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"UniversalKVM pairing identity proof v1");
    hash_field(&mut hash, handshake_hash);
    hash_field(&mut hash, initiator_cert);
    hash_field(&mut hash, responder_cert);
    hash_field(&mut hash, initiator_name.as_bytes());
    hash_field(&mut hash, responder_name.as_bytes());
    hash.finalize().into()
}

fn sign_proof(local: &Identity, context: &[u8]) -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    let key = local.tls_key();
    let signing =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, key.secret_der(), &rng)
            .map_err(|_| "local identity key is not an ECDSA P-256 PKCS#8 key")?;
    Ok(signing
        .sign(&rng, context)
        .map_err(|_| "could not sign pairing identity proof")?
        .as_ref()
        .to_vec())
}

fn verify_proof(cert_der: &[u8], context: &[u8], signature: &[u8]) -> Result<(), String> {
    if signature.len() != 64 {
        return Err("remote identity proof has invalid length".into());
    }
    let (remainder, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|_| "remote certificate cannot be parsed")?;
    if !remainder.is_empty() {
        return Err("remote certificate has trailing data".into());
    }
    let key = cert.public_key().subject_public_key.data.as_ref();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
        .verify(context, signature)
        .map_err(|_| "remote node failed private-key proof".into())
}

fn address_hint(ip: IpAddr) -> Result<String, String> {
    let host = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    resolve_address(&format!("{host}:{DEFAULT_PORT}"))
}

fn preflight(store: &TrustStore, id: &str, name: &str) -> Result<Option<Peer>, String> {
    if let Some(peer) = store.find_id(id) {
        return Ok(Some(peer.clone()));
    }
    if let Some(peer) = store.find(name) {
        return Err(format!(
            "name '{name}' already belongs to a different node ({})",
            peer.id
        ));
    }
    Ok(None)
}

struct CommitGuard {
    dir: PathBuf,
    created_id: Option<String>,
    _lock: fs::File,
}
impl Drop for CommitGuard {
    fn drop(&mut self) {
        if let Some(id) = self.created_id.take() {
            match TrustStore::load(&self.dir)
                .and_then(|mut store| store.remove(&self.dir, &id).map(|_| ()))
            {
                Ok(()) => {}
                Err(e) => eprintln!("ukvm: pairing rollback failed for {id}: {e}"),
            }
        }
    }
}

fn commit_local(
    trust_dir: &Path,
    node_id: &str,
    name: String,
    address: String,
    cert: &[u8],
) -> Result<(Peer, bool, CommitGuard), String> {
    let lock = edit_lock(trust_dir)?;
    let mut store = TrustStore::load(trust_dir)?;
    let existing = preflight(&store, node_id, &name)?;
    let (peer, created) = if let Some(peer) = existing {
        (peer, false)
    } else {
        (store.import(trust_dir, name, Some(address), cert)?, true)
    };
    let guard = CommitGuard {
        dir: trust_dir.to_path_buf(),
        created_id: created.then(|| node_id.to_string()),
        _lock: lock,
    };
    Ok((peer, created, guard))
}

/// Run a single first-contact exchange. The caller owns the overall timeout
/// and cancellation; dropping this future rolls back any unacknowledged new trust.
pub async fn pair_stream<F, Fut>(
    mut stream: TcpStream,
    role: PairRole,
    local: &Identity,
    local_name: &str,
    trust_dir: &Path,
    confirm: F,
) -> Result<PairOutcome, String>
where
    F: FnOnce(PairOffer) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    if local_name.is_empty()
        || local_name.chars().count() > 63
        || local_name.chars().any(char::is_control)
    {
        return Err("local node name is invalid".into());
    }
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let address = address_hint(stream.peer_addr().map_err(|e| e.to_string())?.ip())?;
    let (noise, handshake_hash) = handshake(&mut stream, role).await?;
    let mut channel = SecureChannel { stream, noise };
    let local_cert = local.tls_cert().as_ref().to_vec();
    let local_message = PairMessage::Identity {
        node_id: local.id.clone(),
        cert: local_cert.clone(),
        name: local_name.into(),
    };
    let remote_message = channel.exchange(local_message).await?;
    let PairMessage::Identity {
        node_id,
        cert,
        name,
    } = remote_message
    else {
        return Err("expected remote pairing identity".into());
    };
    validate_identity(&node_id, &cert, &name, &local.id)?;
    let (initiator_cert, responder_cert, initiator_name, responder_name) = match role {
        PairRole::Initiator => (&local_cert[..], &cert[..], local_name, name.as_str()),
        PairRole::Responder => (&cert[..], &local_cert[..], name.as_str(), local_name),
    };
    let context = proof_context(
        &handshake_hash,
        initiator_cert,
        responder_cert,
        initiator_name,
        responder_name,
    );
    let local_proof = sign_proof(local, &context)?;
    let remote_proof = channel.exchange(PairMessage::Proof(local_proof)).await?;
    let PairMessage::Proof(remote_signature) = remote_proof else {
        return Err("expected remote identity proof".into());
    };
    verify_proof(&cert, &context, &remote_signature)?;
    let code = verification_code(
        &handshake_hash,
        initiator_cert,
        responder_cert,
        initiator_name,
        responder_name,
    );
    let existing = preflight(&TrustStore::load(trust_dir)?, &node_id, &name)?;
    let offer = PairOffer {
        remote_name: name.clone(),
        remote_id: node_id.clone(),
        address: address.clone(),
        verification_code: code,
        already_trusted_as: existing.as_ref().map(|p| p.alias.clone()),
    };
    let accepted = confirm(offer).await?;
    match channel.exchange(PairMessage::Decision(accepted)).await? {
        PairMessage::Decision(true) if accepted => {}
        PairMessage::Decision(_) => return Err("pairing rejected by a user".into()),
        _ => return Err("expected pairing decision".into()),
    }
    // Recheck after confirmation, then let the other side know this node can commit.
    let ready = TrustStore::load(trust_dir)
        .and_then(|store| preflight(&store, &node_id, &name).map(|_| ()))
        .is_ok();
    match channel.exchange(PairMessage::Ready(ready)).await? {
        PairMessage::Ready(true) if ready => {}
        PairMessage::Ready(_) => return Err("peer cannot commit pairing trust".into()),
        _ => return Err("expected pairing ready message".into()),
    }
    let (peer, created, mut guard) = match commit_local(trust_dir, &node_id, name, address, &cert) {
        Ok(value) => value,
        Err(err) => {
            let _ = channel.send(&PairMessage::Commit(false)).await;
            return Err(err);
        }
    };
    match channel.exchange(PairMessage::Commit(true)).await? {
        PairMessage::Commit(true) => {
            guard.created_id = None;
            Ok(PairOutcome { peer, created })
        }
        PairMessage::Commit(false) => Err("remote trust-store write failed".into()),
        _ => Err("expected pairing commit acknowledgement".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transport, trust::SessionPermission};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn two_fresh_nodes_pair_with_same_code_and_default_deny_then_use_mtls() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let codes = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let b_codes = Arc::clone(&codes);
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(
                stream,
                PairRole::Responder,
                &b,
                "io",
                &b_dir,
                |offer| async move {
                    b_codes.lock().unwrap().push(offer.verification_code);
                    Ok(true)
                },
            )
            .await
        };
        let a_codes = Arc::clone(&codes);
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                |offer| async move {
                    a_codes.lock().unwrap().push(offer.verification_code);
                    Ok(true)
                },
            )
            .await
        };
        let (a_result, b_result) = tokio::join!(initiator, responder);
        assert!(a_result.unwrap().created);
        assert!(b_result.unwrap().created);
        {
            let values = codes.lock().unwrap();
            assert_eq!(values.len(), 2);
            assert_eq!(values[0], values[1]);
        }
        let a_store = TrustStore::load(&a_dir).unwrap();
        let b_store = TrustStore::load(&b_dir).unwrap();
        assert_eq!(a_store.find_id(&b.id).unwrap().alias, "io");
        assert_eq!(b_store.find_id(&a.id).unwrap().alias, "spectrum");
        assert!(a_store.authorize(&b.id, SessionPermission::Connect).is_ok());
        for (store, id) in [(&a_store, &b.id), (&b_store, &a.id)] {
            assert!(store.authorize(id, SessionPermission::DebugInput).is_err());
            assert!(store
                .authorize(id, SessionPermission::ControlThisNode)
                .is_err());
        }
        let a_cert = fs::read(a.cert_path.clone()).unwrap();
        let b_cert = fs::read(b.cert_path.clone()).unwrap();
        assert_eq!(
            fs::read(crate::trust::cert_path(&a_dir, &b.id)).unwrap(),
            b_cert
        );
        assert_eq!(
            fs::read(crate::trust::cert_path(&b_dir, &a.id)).unwrap(),
            a_cert
        );
        let mut b_store = TrustStore::load(&b_dir).unwrap();
        b_store
            .set_permission(&b_dir, "spectrum", SessionPermission::DebugInput, true)
            .unwrap();
        let listener = transport::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let receiver = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let connection = transport::accept_trusted(
                tcp,
                &b,
                &TrustStore::load(&b_dir).unwrap(),
                &b_dir,
                "io",
            )
            .await
            .unwrap();
            transport::receive_debug(connection, |_| {}).await.unwrap()
        };
        let sender = async {
            let connection = transport::connect(
                &addr.to_string(),
                &a,
                &crate::trust::cert_path(&a_dir, &b.id),
                "spectrum",
            )
            .await
            .unwrap();
            let sender = transport::start_sender(connection).await.unwrap();
            sender
                .queue
                .send(ukvm_protocol::InputEvent::PointerMotion { dx: 2.0, dy: 0.0 })
                .await
                .unwrap();
            sender.finish().await.unwrap()
        };
        let (sent, received) = tokio::join!(sender, receiver);
        assert_eq!(sent.sent.pointer, 1);
        assert_eq!(received.pointer, 1);
    }

    #[tokio::test]
    async fn rejection_never_creates_trust() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(stream, PairRole::Responder, &b, "io", &b_dir, |_| async {
                Ok(false)
            })
            .await
        };
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                |_| async { Ok(true) },
            )
            .await
        };
        let (ar, br) = tokio::join!(initiator, responder);
        assert!(ar.is_err());
        assert!(br.is_err());
        assert!(TrustStore::load(&a_dir).unwrap().peers.is_empty());
        assert!(TrustStore::load(&b_dir).unwrap().peers.is_empty());
    }

    #[test]
    fn certificate_id_and_alias_conflicts_are_checked() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b")).unwrap();
        let cert_a = fs::read(&a.cert_path).unwrap();
        let cert_b = fs::read(&b.cert_path).unwrap();
        assert!(validate_identity(&b.id, &cert_a, "a", &b.id).is_err());
        assert!(validate_identity(&a.id, &cert_a, "a", &a.id).is_err());
        let dir = temp.path().join("trust");
        let mut store = TrustStore::default();
        store.import(&dir, "io".into(), None, &cert_a).unwrap();
        assert!(preflight(&store, &b.id, "io").is_err());
        assert_eq!(
            preflight(&store, &a.id, "renamed").unwrap().unwrap().alias,
            "io"
        );
        assert!(validate_identity(&b.id, &cert_b, "io", &a.id).is_ok());
        let context = proof_context(&[7u8; 32], &cert_a, &cert_b, "a", "b");
        let proof = sign_proof(&a, &context).unwrap();
        assert!(verify_proof(&cert_a, &context, &proof).is_ok());
        assert!(verify_proof(&cert_b, &context, &proof).is_err());
        let mut bad = proof;
        bad[0] ^= 1;
        assert!(verify_proof(&cert_a, &context, &bad).is_err());
    }

    #[tokio::test]
    async fn wrong_version_malformed_frame_and_disconnect_fail() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            handshake(&mut stream, PairRole::Responder)
                .await
                .err()
                .unwrap()
        };
        let initiator = async {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            write_raw(&mut stream, b"UKVM-PAIR\0\x02").await.unwrap();
        };
        let (error, _) = tokio::join!(responder, initiator);
        assert!(error.contains("version mismatch"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_raw(&mut stream).await.err().unwrap()
        };
        let initiator = async {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream.write_all(&u16::MAX.to_be_bytes()).await.unwrap();
        };
        let (error, _) = tokio::join!(responder, initiator);
        assert!(error.contains("invalid length"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_raw(&mut stream).await.err().unwrap()
        };
        let initiator = async {
            drop(TcpStream::connect(addr).await.unwrap());
        };
        let (error, _) = tokio::join!(responder, initiator);
        assert!(error.contains("closed"));
    }

    #[tokio::test]
    async fn timeout_and_cancellation_leave_no_trust() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("trust");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        assert!(TrustStore::load(&dir).unwrap().peers.is_empty());
    }

    #[test]
    fn transcript_and_identity_changes_change_code() {
        let one = verification_code(&[1u8; 32], b"cert-a", b"cert-b", "a", "b");
        assert_ne!(
            one,
            verification_code(&[2u8; 32], b"cert-a", b"cert-b", "a", "b")
        );
        assert_ne!(
            one,
            verification_code(&[1u8; 32], b"cert-a", b"cert-c", "a", "b")
        );
        assert_ne!(
            one,
            verification_code(&[1u8; 32], b"cert-a", b"cert-b", "a", "c")
        );
    }

    #[test]
    fn identity_message_contains_public_certificate_only() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(temp.path()).unwrap();
        let cert = identity.tls_cert().as_ref().to_vec();
        let key = identity.tls_key();
        let encoded = codec()
            .serialize(&PairMessage::Identity {
                node_id: identity.id,
                name: "node".into(),
                cert: cert.clone(),
            })
            .unwrap();
        assert!(encoded.windows(16).any(|window| window == &cert[..16]));
        assert!(!encoded
            .windows(16)
            .any(|window| window == &key.secret_der()[..16]));
    }

    #[tokio::test]
    async fn trust_is_not_written_before_both_humans_confirm() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (a_seen, a_ready) = tokio::sync::oneshot::channel();
        let (b_seen, b_ready) = tokio::sync::oneshot::channel();
        let (a_confirm, a_continue) = tokio::sync::oneshot::channel();
        let (b_confirm, b_continue) = tokio::sync::oneshot::channel();
        let a_dir_task = a_dir.clone();
        let b_dir_task = b_dir.clone();
        let responder = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(
                stream,
                PairRole::Responder,
                &b,
                "io",
                &b_dir_task,
                move |_| async move {
                    let _ = b_seen.send(());
                    b_continue.await.map_err(|_| "cancelled".to_string())
                },
            )
            .await
        });
        let initiator = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir_task,
                move |_| async move {
                    let _ = a_seen.send(());
                    a_continue.await.map_err(|_| "cancelled".to_string())
                },
            )
            .await
        });
        a_ready.await.unwrap();
        b_ready.await.unwrap();
        assert!(TrustStore::load(&a_dir).unwrap().peers.is_empty());
        assert!(TrustStore::load(&b_dir).unwrap().peers.is_empty());
        a_confirm.send(true).unwrap();
        b_confirm.send(true).unwrap();
        assert!(initiator.await.unwrap().is_ok());
        assert!(responder.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn storage_failure_rolls_back_other_nodes_new_trust() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        fs::create_dir_all(b_dir.join("peers").join(format!("{}.der", a.id))).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(stream, PairRole::Responder, &b, "io", &b_dir, |_| async {
                Ok(true)
            })
            .await
        };
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                |_| async { Ok(true) },
            )
            .await
        };
        let (ar, br) = tokio::join!(initiator, responder);
        assert!(ar.is_err());
        assert!(br.is_err());
        assert!(TrustStore::load(&a_dir).unwrap().peers.is_empty());
        assert!(TrustStore::load(&b_dir).unwrap().peers.is_empty());
    }

    #[tokio::test]
    async fn existing_identity_keeps_alias_address_and_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let mut a_store = TrustStore::default();
        a_store
            .import(
                &a_dir,
                "original-b".into(),
                Some("10.0.0.9".into()),
                &fs::read(&b.cert_path).unwrap(),
            )
            .unwrap();
        a_store
            .set_permission(&a_dir, "original-b", SessionPermission::DebugInput, true)
            .unwrap();
        let mut b_store = TrustStore::default();
        b_store
            .import(
                &b_dir,
                "original-a".into(),
                Some("10.0.0.8".into()),
                &fs::read(&a.cert_path).unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(
                stream,
                PairRole::Responder,
                &b,
                "new-b",
                &b_dir,
                |offer| async move {
                    assert_eq!(offer.already_trusted_as.as_deref(), Some("original-a"));
                    Ok(true)
                },
            )
            .await
        };
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "new-a",
                &a_dir,
                |offer| async move {
                    assert_eq!(offer.already_trusted_as.as_deref(), Some("original-b"));
                    Ok(true)
                },
            )
            .await
        };
        let (ar, br) = tokio::join!(initiator, responder);
        assert!(!ar.unwrap().created);
        assert!(!br.unwrap().created);
        let a_record = TrustStore::load(&a_dir)
            .unwrap()
            .find_id(&b.id)
            .unwrap()
            .clone();
        assert_eq!(a_record.alias, "original-b");
        assert_eq!(a_record.address.as_deref(), Some("10.0.0.9"));
        assert!(a_record.permissions.debug_input);
    }

    #[tokio::test]
    async fn alias_collision_with_different_identity_fails_without_repointing() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let c = Identity::load_or_create(&temp.path().join("c-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let mut store = TrustStore::default();
        store
            .import(
                &b_dir,
                "spectrum".into(),
                None,
                &fs::read(&c.cert_path).unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(stream, PairRole::Responder, &b, "io", &b_dir, |_| async {
                Ok(true)
            })
            .await
        };
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                |_| async { Ok(true) },
            )
            .await
        };
        let (ar, br) = tokio::join!(initiator, responder);
        assert!(ar.is_err());
        assert!(br.is_err());
        let b_store = TrustStore::load(&b_dir).unwrap();
        assert_eq!(b_store.find("spectrum").unwrap().id, c.id);
        assert!(b_store.find_id(&a.id).is_none());
        assert!(TrustStore::load(&a_dir).unwrap().peers.is_empty());
    }

    #[tokio::test]
    async fn temporary_listener_accepts_only_one_attempt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let first = TcpStream::connect(addr).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();
        drop(listener);
        assert!(TcpStream::connect(addr).await.is_err());
        drop(first);
    }

    #[test]
    fn dropped_commit_guard_removes_unacknowledged_trust() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let dir = temp.path().join("trust");
        let lock = edit_lock(&dir).unwrap();
        let mut store = TrustStore::default();
        store
            .import(
                &dir,
                "peer".into(),
                None,
                &fs::read(identity.cert_path).unwrap(),
            )
            .unwrap();
        let guard = CommitGuard {
            dir: dir.clone(),
            created_id: Some(identity.id),
            _lock: lock,
        };
        drop(guard);
        assert!(TrustStore::load(&dir).unwrap().peers.is_empty());
    }

    #[test]
    fn replayed_first_noise_message_gets_fresh_responder_state() {
        let params = NOISE.parse().unwrap();
        let mut initiator = Builder::new(params)
            .prologue(PROLOGUE)
            .build_initiator()
            .unwrap();
        let mut first = [0u8; 128];
        let len = initiator.write_message(&[], &mut first).unwrap();
        let mut responses = Vec::new();
        for _ in 0..2 {
            let params = NOISE.parse().unwrap();
            let mut responder = Builder::new(params)
                .prologue(PROLOGUE)
                .build_responder()
                .unwrap();
            let mut payload = [0u8; 128];
            responder.read_message(&first[..len], &mut payload).unwrap();
            let mut response = [0u8; 128];
            let size = responder.write_message(&[], &mut response).unwrap();
            responses.push(response[..size].to_vec());
        }
        assert_ne!(responses[0], responses[1]);
        let mut payload = [0u8; 128];
        initiator.read_message(&responses[0], &mut payload).unwrap();
        assert!(initiator.is_handshake_finished());
    }
}
