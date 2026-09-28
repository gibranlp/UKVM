//! First-contact trust bootstrap. Noise supplies ephemeral encryption; users
//! authenticate the exchange by comparing a transcript- and identity-bound SAS.
use crate::{
    identity::{certificate_id, Identity},
    pairing_state::{CommitState, PendingPairing},
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
use std::{future::Future, net::IpAddr, path::Path};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub const PAIRING_VERSION: u8 = 2;
pub const PAIRING_PORT: u16 = 24801;
pub const PAIRING_TIMEOUT_SECS: u64 = 90;
const MAGIC: &[u8] = b"UKVM-PAIR\0\x02";
const NOISE: &str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"UniversalKVM pairing protocol 2";
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairFailureKind {
    Rejected,
    Failed,
    Uncertain,
    Conflict,
}

#[derive(Debug)]
pub struct PairFailure {
    pub kind: PairFailureKind,
    pub message: String,
}

impl PairFailure {
    fn new(kind: PairFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<String> for PairFailure {
    fn from(message: String) -> Self {
        Self::new(PairFailureKind::Failed, message)
    }
}

impl From<&str> for PairFailure {
    fn from(message: &str) -> Self {
        Self::new(PairFailureKind::Failed, message)
    }
}

impl std::fmt::Display for PairFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for PairFailure {}

#[derive(Clone, Debug)]
pub enum RecoveryOutcome {
    Completed(Peer),
    RestartRequired,
}

#[derive(Deserialize, Serialize)]
enum PairMessage {
    Mode(PairMode),
    Identity {
        node_id: String,
        cert: Vec<u8>,
        name: String,
    },
    Proof(Vec<u8>),
    Decision(bool),
    Ready(bool),
    Commit(bool),
    RecoveryStatus {
        transaction_id: String,
        committed: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
enum PairMode {
    New,
    Recover(String),
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

fn transaction_id(hash: &[u8], initiator_cert: &[u8], responder_cert: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"UniversalKVM pairing transaction v1");
    hash_field(&mut digest, hash);
    hash_field(&mut digest, initiator_cert);
    hash_field(&mut digest, responder_cert);
    format!("{:x}", digest.finalize())
}

struct Authenticated {
    channel: SecureChannel,
    hash: Vec<u8>,
    remote_id: String,
    remote_cert: Vec<u8>,
    remote_name: String,
    address: String,
    mode: PairMode,
}

async fn authenticate(
    mut stream: TcpStream,
    role: PairRole,
    local: &Identity,
    local_name: &str,
    expected: PairMode,
) -> Result<Authenticated, String> {
    if local_name.is_empty()
        || local_name.chars().count() > 63
        || local_name.chars().any(char::is_control)
    {
        return Err("local node name is invalid".into());
    }
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let address = address_hint(stream.peer_addr().map_err(|e| e.to_string())?.ip())?;
    let (noise, hash) = handshake(&mut stream, role).await?;
    let mut channel = SecureChannel { stream, noise };
    let mode = match role {
        PairRole::Initiator => {
            channel.send(&PairMessage::Mode(expected.clone())).await?;
            match channel.receive().await? {
                PairMessage::Mode(mode) if mode == expected => mode,
                _ => return Err("pairing mode mismatch".into()),
            }
        }
        PairRole::Responder => {
            let PairMessage::Mode(mode) = channel.receive().await? else {
                return Err("expected pairing mode".into());
            };
            let allowed = matches!(
                (&mode, &expected),
                (PairMode::New, PairMode::New) | (PairMode::Recover(_), PairMode::Recover(_))
            );
            if !allowed {
                return Err("pairing mode mismatch".into());
            }
            channel.send(&PairMessage::Mode(mode.clone())).await?;
            mode
        }
    };
    let local_cert = local.tls_cert().as_ref().to_vec();
    let PairMessage::Identity {
        node_id,
        cert,
        name,
    } = channel
        .exchange(PairMessage::Identity {
            node_id: local.id.clone(),
            cert: local_cert.clone(),
            name: local_name.into(),
        })
        .await?
    else {
        return Err("expected remote pairing identity".into());
    };
    validate_identity(&node_id, &cert, &name, &local.id)?;
    let (initiator_cert, responder_cert, initiator_name, responder_name) = match role {
        PairRole::Initiator => (&local_cert[..], &cert[..], local_name, name.as_str()),
        PairRole::Responder => (&cert[..], &local_cert[..], name.as_str(), local_name),
    };
    let context = proof_context(
        &hash,
        initiator_cert,
        responder_cert,
        initiator_name,
        responder_name,
    );
    let PairMessage::Proof(signature) = channel
        .exchange(PairMessage::Proof(sign_proof(local, &context)?))
        .await?
    else {
        return Err("expected remote identity proof".into());
    };
    verify_proof(&cert, &context, &signature)?;
    Ok(Authenticated {
        channel,
        hash,
        remote_id: node_id,
        remote_cert: cert,
        remote_name: name,
        address,
        mode,
    })
}

fn commit_local(trust_dir: &Path, pending: &mut PendingPairing) -> Result<(Peer, bool), String> {
    let _lock = edit_lock(trust_dir)?;
    let durable = PendingPairing::load(trust_dir, &pending.transaction_id)?;
    if durable.local_id != pending.local_id
        || durable.remote_id != pending.remote_id
        || durable.remote_certificate != pending.remote_certificate
        || durable.state != pending.state
    {
        return Err("pairing recovery evidence changed before trust commit".into());
    }
    let mut store = TrustStore::load(trust_dir)?;
    let existing = preflight(&store, &pending.remote_id, &pending.remote_name)?;
    let (peer, created) = if let Some(peer) = existing {
        let saved = std::fs::read(crate::trust::cert_path(trust_dir, &peer.id))
            .map_err(|e| e.to_string())?;
        if saved != pending.remote_certificate {
            return Err("trusted certificate conflicts with pairing".into());
        }
        (peer, false)
    } else {
        (
            store.import(
                trust_dir,
                pending.remote_name.clone(),
                Some(pending.remote_address.clone()),
                &pending.remote_certificate,
            )?,
            true,
        )
    };
    // The Prepared record was durable before this write. If this update fails or
    // the process dies, recovery compares that record with the actual trust store.
    pending.state = CommitState::CommittedLocally;
    pending.save_locked(trust_dir)?;
    Ok((peer, created))
}

fn complete_local(trust_dir: &Path, pending: &mut PendingPairing) -> Result<(), String> {
    let _lock = edit_lock(trust_dir)?;
    if pending.local_peer(trust_dir)?.is_none() {
        return Err("trusted peer disappeared before completion".into());
    }
    pending.state = CommitState::Complete;
    pending.save_locked(trust_dir)
}

/// Run a single first-contact exchange. Once both users confirm, interrupted
/// commits retain durable recovery evidence instead of deleting trusted peers.
pub async fn pair_stream<F, Fut>(
    stream: TcpStream,
    role: PairRole,
    local: &Identity,
    local_name: &str,
    trust_dir: &Path,
    confirm: F,
) -> Result<PairOutcome, PairFailure>
where
    F: FnOnce(PairOffer) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    pair_stream_inner(
        stream,
        role,
        local,
        local_name,
        trust_dir,
        confirm,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    AfterLocalConfirmation,
    AfterMutualConfirmation,
    AfterPendingWrite,
    BeforeLocalCommit,
    AfterLocalCommit,
    AfterCommitSend,
}

async fn pair_stream_inner<F, Fut>(
    stream: TcpStream,
    role: PairRole,
    local: &Identity,
    local_name: &str,
    trust_dir: &Path,
    confirm: F,
    #[cfg(test)] fault: Option<FaultPoint>,
) -> Result<PairOutcome, PairFailure>
where
    F: FnOnce(PairOffer) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    let Authenticated {
        mut channel,
        hash: handshake_hash,
        remote_id: node_id,
        remote_cert: cert,
        remote_name: name,
        address,
        ..
    } = authenticate(stream, role, local, local_name, PairMode::New).await?;
    let local_cert = local.tls_cert().as_ref().to_vec();
    let (initiator_cert, responder_cert, initiator_name, responder_name) = match role {
        PairRole::Initiator => (&local_cert[..], &cert[..], local_name, name.as_str()),
        PairRole::Responder => (&cert[..], &local_cert[..], name.as_str(), local_name),
    };
    let tx = transaction_id(&handshake_hash, initiator_cert, responder_cert);
    let code = verification_code(
        &handshake_hash,
        initiator_cert,
        responder_cert,
        initiator_name,
        responder_name,
    );
    let existing = preflight(&TrustStore::load(trust_dir)?, &node_id, &name)
        .map_err(|e| PairFailure::new(PairFailureKind::Conflict, e))?;
    let offer = PairOffer {
        remote_name: name.clone(),
        remote_id: node_id.clone(),
        address: address.clone(),
        verification_code: code,
        already_trusted_as: existing.as_ref().map(|p| p.alias.clone()),
    };
    let accepted = confirm(offer).await?;
    #[cfg(test)]
    if fault == Some(FaultPoint::AfterLocalConfirmation) {
        return Err("injected interruption after local confirmation".into());
    }
    match channel.exchange(PairMessage::Decision(accepted)).await? {
        PairMessage::Decision(true) if accepted => {}
        PairMessage::Decision(_) => {
            return Err(PairFailure::new(
                PairFailureKind::Rejected,
                "pairing rejected by a user",
            ))
        }
        _ => return Err("expected pairing decision".into()),
    }
    #[cfg(test)]
    if fault == Some(FaultPoint::AfterMutualConfirmation) {
        return Err("injected interruption after mutual confirmation".into());
    }
    // Mutual confirmation is durable before either side is allowed to commit.
    let mut pending = PendingPairing::new(
        tx.clone(),
        local.id.clone(),
        node_id.clone(),
        cert,
        name,
        address,
    )?;
    let ready = TrustStore::load(trust_dir)
        .and_then(|store| preflight(&store, &node_id, &pending.remote_name).map(|_| ()))
        .and_then(|_| pending.save(trust_dir))
        .is_ok();
    #[cfg(test)]
    if fault == Some(FaultPoint::AfterPendingWrite) {
        return Err("injected interruption after pending write".into());
    }
    match channel.exchange(PairMessage::Ready(ready)).await? {
        PairMessage::Ready(true) if ready => {}
        PairMessage::Ready(_) => {
            if ready {
                let _ = pending.remove(trust_dir);
            }
            return Err("pairing failed before trust commit; retry a new ceremony".into());
        }
        _ => return Err("expected pairing ready message".into()),
    }
    #[cfg(test)]
    if fault == Some(FaultPoint::BeforeLocalCommit) {
        return Err("injected interruption before local commit".into());
    }
    let (peer, created) = match commit_local(trust_dir, &mut pending) {
        Ok(value) => value,
        Err(err) => {
            let _ = channel.send(&PairMessage::Commit(false)).await;
            return Err(PairFailure::new(PairFailureKind::Uncertain, format!("pairing commit failed or is uncertain (transaction {tx}): {err}; run 'ukvm pair status'")));
        }
    };
    #[cfg(test)]
    if fault == Some(FaultPoint::AfterLocalCommit) {
        return Err("injected interruption after local commit".into());
    }
    channel.send(&PairMessage::Commit(true)).await.map_err(|err| PairFailure::new(PairFailureKind::Uncertain, format!("pairing outcome uncertain (transaction {tx}): {err}; run 'ukvm pair status' and recover")))?;
    #[cfg(test)]
    if fault == Some(FaultPoint::AfterCommitSend) {
        return Err("injected interruption after commit send".into());
    }
    match channel.receive().await {
        Err(err) => Err(PairFailure::new(PairFailureKind::Uncertain, format!("pairing outcome uncertain (transaction {tx}): {err}; run 'ukvm pair status' and recover"))),
        Ok(PairMessage::Commit(true)) => {
            complete_local(trust_dir, &mut pending).map_err(|err| PairFailure::new(PairFailureKind::Uncertain, format!("both commits observed, but local receipt update failed (transaction {tx}): {err}")))?;
            Ok(PairOutcome { peer, created })
        }
        Ok(PairMessage::Commit(false)) => Err(PairFailure::new(PairFailureKind::Uncertain, format!("remote trust-store write failed; local commit may need recovery (transaction {tx}); run 'ukvm pair status'"))),
        Ok(_) => Err(PairFailure::new(PairFailureKind::Uncertain, format!("pairing outcome uncertain (transaction {tx}): invalid final acknowledgement; run 'ukvm pair status'"))),
    }
}

/// Reconcile only a previously confirmed, unexpired transaction. The fresh
/// Noise channel and P-256 identity proofs authenticate both pending records.
pub async fn recover_stream(
    stream: TcpStream,
    role: PairRole,
    local: &Identity,
    local_name: &str,
    trust_dir: &Path,
    transaction: Option<&str>,
) -> Result<RecoveryOutcome, String> {
    let expected = PairMode::Recover(transaction.unwrap_or_default().to_string());
    if role == PairRole::Initiator && transaction.is_none() {
        return Err("recovery initiator needs a transaction ID".into());
    }
    let Authenticated {
        mut channel,
        remote_id,
        remote_cert,
        mode,
        ..
    } = authenticate(stream, role, local, local_name, expected).await?;
    let PairMode::Recover(tx) = mode else {
        return Err("expected recovery mode".into());
    };
    let mut pending = PendingPairing::load(trust_dir, &tx)?;
    if pending.local_id != local.id
        || pending.remote_id != remote_id
        || pending.remote_certificate != remote_cert
    {
        return Err("recovery transaction identity or certificate mismatch".into());
    }
    let committed = pending.local_peer(trust_dir)?.is_some();
    if !committed && pending.state != CommitState::Prepared {
        return Err(
            "previously committed trust was removed; start a new human-confirmed ceremony".into(),
        );
    }
    let PairMessage::RecoveryStatus {
        transaction_id: peer_tx,
        committed: remote_committed,
    } = channel
        .exchange(PairMessage::RecoveryStatus {
            transaction_id: tx.clone(),
            committed,
        })
        .await?
    else {
        return Err("expected recovery status".into());
    };
    if peer_tx != tx {
        return Err("recovery transaction mismatch".into());
    }
    if !committed && !remote_committed {
        return Ok(RecoveryOutcome::RestartRequired);
    }
    let peer = if committed {
        pending
            .local_peer(trust_dir)?
            .ok_or("trusted peer disappeared during recovery")?
    } else {
        commit_local(trust_dir, &mut pending)?.0
    };
    match channel.exchange(PairMessage::Commit(true)).await {
        Ok(PairMessage::Commit(true)) => {
            complete_local(trust_dir, &mut pending)?;
            Ok(RecoveryOutcome::Completed(peer))
        }
        Ok(PairMessage::Commit(false)) => Err(format!(
            "remote recovery commit failed; transaction {tx} remains uncertain"
        )),
        Ok(_) => Err(format!(
            "invalid recovery acknowledgement; transaction {tx} remains uncertain"
        )),
        Err(err) => Err(format!(
            "recovery acknowledgement lost for transaction {tx}: {err}; retry recovery"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transport, trust::SessionPermission};
    use std::{
        fs,
        sync::{Arc, Mutex},
    };
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
        assert_eq!(ar.unwrap_err().kind, PairFailureKind::Rejected);
        assert_eq!(br.unwrap_err().kind, PairFailureKind::Rejected);
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
            write_raw(&mut stream, b"UKVM-PAIR\0\x01").await.unwrap();
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
    async fn storage_failure_leaves_detectable_one_sided_trust_for_recovery() {
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
        assert!(TrustStore::load(&a_dir).unwrap().find_id(&b.id).is_some());
        assert!(TrustStore::load(&b_dir).unwrap().peers.is_empty());
        assert_eq!(
            crate::pairing_state::list(&a_dir).unwrap()[0].status(&a_dir),
            crate::pairing_state::LocalStatus::CommittedUnconfirmed
        );
        assert_eq!(
            crate::pairing_state::list(&b_dir).unwrap()[0].status(&b_dir),
            crate::pairing_state::LocalStatus::Prepared
        );
    }

    async fn recovery_trial(
        a_committed: bool,
        b_committed: bool,
    ) -> (RecoveryOutcome, RecoveryOutcome, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let tx = transaction_id(&[7; 32], a.tls_cert().as_ref(), b.tls_cert().as_ref());
        let a_record = PendingPairing::new(
            tx.clone(),
            a.id.clone(),
            b.id.clone(),
            b.tls_cert().as_ref().to_vec(),
            "io".into(),
            "127.0.0.1:24800".into(),
        )
        .unwrap();
        let b_record = PendingPairing::new(
            tx.clone(),
            b.id.clone(),
            a.id.clone(),
            a.tls_cert().as_ref().to_vec(),
            "spectrum".into(),
            "127.0.0.1:24800".into(),
        )
        .unwrap();
        a_record.save(&a_dir).unwrap();
        b_record.save(&b_dir).unwrap();
        if a_committed {
            let mut store = TrustStore::default();
            store
                .import(
                    &a_dir,
                    "io".into(),
                    Some("10.0.0.9".into()),
                    b.tls_cert().as_ref(),
                )
                .unwrap();
            store
                .set_permission(&a_dir, "io", SessionPermission::DebugInput, true)
                .unwrap();
        }
        if b_committed {
            TrustStore::default()
                .import(&b_dir, "spectrum".into(), None, a.tls_cert().as_ref())
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            recover_stream(stream, PairRole::Responder, &b, "io", &b_dir, None).await
        };
        let initiator = async {
            let stream = TcpStream::connect(addr).await.unwrap();
            recover_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                Some(&tx),
            )
            .await
        };
        let (ar, br) = tokio::join!(initiator, responder);
        (ar.unwrap(), br.unwrap(), temp)
    }

    #[tokio::test]
    async fn recovery_reconciles_either_one_sided_commit_and_preserves_permissions() {
        for (a_committed, b_committed) in [(true, false), (false, true), (true, true)] {
            let (ar, br, temp) = recovery_trial(a_committed, b_committed).await;
            assert!(matches!(ar, RecoveryOutcome::Completed(_)));
            assert!(matches!(br, RecoveryOutcome::Completed(_)));
            let a_dir = temp.path().join("a-trust");
            let b_dir = temp.path().join("b-trust");
            let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
            let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
            let a_peer = TrustStore::load(&a_dir)
                .unwrap()
                .find_id(&b.id)
                .unwrap()
                .clone();
            let b_peer = TrustStore::load(&b_dir)
                .unwrap()
                .find_id(&a.id)
                .unwrap()
                .clone();
            assert_eq!(a_peer.permissions.debug_input, a_committed);
            assert!(!b_peer.permissions.debug_input);
            assert!(!a_peer.permissions.control_this_node);
            assert!(!b_peer.permissions.control_this_node);
            assert_eq!(
                a_peer.address.as_deref(),
                Some(if a_committed {
                    "10.0.0.9"
                } else {
                    "127.0.0.1:24800"
                })
            );
            assert_eq!(
                crate::pairing_state::list(&a_dir).unwrap()[0].status(&a_dir),
                crate::pairing_state::LocalStatus::Complete
            );
            assert_eq!(
                crate::pairing_state::list(&b_dir).unwrap()[0].status(&b_dir),
                crate::pairing_state::LocalStatus::Complete
            );
        }
    }

    #[tokio::test]
    async fn recovery_of_neither_committed_requires_new_ceremony() {
        let (ar, br, temp) = recovery_trial(false, false).await;
        assert!(matches!(ar, RecoveryOutcome::RestartRequired));
        assert!(matches!(br, RecoveryOutcome::RestartRequired));
        for dir in [temp.path().join("a-trust"), temp.path().join("b-trust")] {
            assert!(TrustStore::load(&dir).unwrap().peers.is_empty());
            assert_eq!(crate::pairing_state::list(&dir).unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn recovery_rejects_unrelated_transaction_and_missing_remote_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-identity")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-identity")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let tx = "a".repeat(64);
        let record = PendingPairing::new(
            tx.clone(),
            a.id.clone(),
            b.id.clone(),
            b.tls_cert().as_ref().to_vec(),
            "io".into(),
            "127.0.0.1:24800".into(),
        )
        .unwrap();
        record.save(&a_dir).unwrap();
        TrustStore::default()
            .import(&a_dir, "io".into(), None, b.tls_cert().as_ref())
            .unwrap();
        for claimed_tx in [tx.as_str(), &"b".repeat(64)] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let responder = async {
                let (stream, _) = listener.accept().await.unwrap();
                recover_stream(stream, PairRole::Responder, &b, "io", &b_dir, None).await
            };
            let initiator = async {
                let stream = TcpStream::connect(addr).await.unwrap();
                recover_stream(
                    stream,
                    PairRole::Initiator,
                    &a,
                    "spectrum",
                    &a_dir,
                    Some(claimed_tx),
                )
                .await
            };
            let (ar, br) = tokio::join!(initiator, responder);
            assert!(ar.is_err());
            assert!(br.is_err());
            assert!(TrustStore::load(&b_dir).unwrap().peers.is_empty());
        }
        assert!(TrustStore::load(&a_dir).unwrap().find_id(&b.id).is_some());
    }

    #[tokio::test]
    async fn injected_commit_boundary_interruptions_are_inspectable_and_safe() {
        for point in [
            FaultPoint::AfterLocalConfirmation,
            FaultPoint::AfterMutualConfirmation,
            FaultPoint::AfterPendingWrite,
            FaultPoint::BeforeLocalCommit,
            FaultPoint::AfterLocalCommit,
            FaultPoint::AfterCommitSend,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let a = Identity::load_or_create(&temp.path().join("a-id")).unwrap();
            let b = Identity::load_or_create(&temp.path().join("b-id")).unwrap();
            let a_dir = temp.path().join("a-trust");
            let b_dir = temp.path().join("b-trust");
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
                pair_stream_inner(
                    stream,
                    PairRole::Initiator,
                    &a,
                    "spectrum",
                    &a_dir,
                    |_| async { Ok(true) },
                    Some(point),
                )
                .await
            };
            let (ar, _br) = tokio::join!(initiator, responder);
            assert!(ar.is_err(), "{point:?}");
            let a_store = TrustStore::load(&a_dir).unwrap();
            let b_store = TrustStore::load(&b_dir).unwrap();
            for (store, remote) in [(&a_store, &b.id), (&b_store, &a.id)] {
                assert!(
                    store
                        .authorize(remote, SessionPermission::DebugInput)
                        .is_err(),
                    "{point:?}"
                );
                assert!(
                    store
                        .authorize(remote, SessionPermission::ControlThisNode)
                        .is_err(),
                    "{point:?}"
                );
            }
            let a_records = crate::pairing_state::list(&a_dir).unwrap();
            let b_records = crate::pairing_state::list(&b_dir).unwrap();
            if a_store.find_id(&b.id).is_some() {
                assert_eq!(a_records.len(), 1, "{point:?}");
            }
            if b_store.find_id(&a.id).is_some() {
                assert_eq!(b_records.len(), 1, "{point:?}");
            }
            if matches!(
                point,
                FaultPoint::AfterLocalConfirmation
                    | FaultPoint::AfterMutualConfirmation
                    | FaultPoint::AfterPendingWrite
            ) {
                assert!(a_store.peers.is_empty(), "{point:?}");
            }
            if matches!(
                point,
                FaultPoint::AfterLocalCommit | FaultPoint::AfterCommitSend
            ) {
                assert!(a_store.find_id(&b.id).is_some(), "{point:?}");
            }
        }
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
    fn committed_trust_survives_lost_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let local = Identity::load_or_create(&temp.path().join("local")).unwrap();
        let dir = temp.path().join("trust");
        let tx = transaction_id(
            &[9; 32],
            local.tls_cert().as_ref(),
            identity.tls_cert().as_ref(),
        );
        let mut pending = PendingPairing::new(
            tx.clone(),
            local.id,
            identity.id.clone(),
            fs::read(identity.cert_path).unwrap(),
            "peer".into(),
            "127.0.0.1:24800".into(),
        )
        .unwrap();
        pending.save(&dir).unwrap();
        commit_local(&dir, &mut pending).unwrap();
        drop(pending);
        assert!(TrustStore::load(&dir)
            .unwrap()
            .find_id(&identity.id)
            .is_some());
        assert_eq!(
            PendingPairing::load(&dir, &tx).unwrap().status(&dir),
            crate::pairing_state::LocalStatus::CommittedUnconfirmed
        );
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
