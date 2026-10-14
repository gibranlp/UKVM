//! Durable evidence of a mutually confirmed pairing. These records grant no permission.
use crate::{
    identity::certificate_id,
    trust::{atomic_write, cert_path, edit_lock, resolve_address, Peer, TrustStore},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    Prepared,
    CommittedLocally,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingPairing {
    pub version: u8,
    pub transaction_id: String,
    pub local_id: String,
    pub remote_id: String,
    pub remote_certificate: Vec<u8>,
    pub remote_name: String,
    pub remote_address: String,
    pub mutually_confirmed: bool,
    pub created_at: u64,
    pub state: CommitState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalStatus {
    Prepared,
    CommittedUnconfirmed,
    Complete,
    Conflict,
    Expired,
}

fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}

fn record_path(dir: &Path, tx: &str) -> Result<PathBuf, String> {
    if tx.len() != 64 || !tx.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid pairing transaction ID".into());
    }
    Ok(dir.join("pairing").join(format!("{tx}.toml")))
}

impl PendingPairing {
    pub fn new(
        tx: String,
        local_id: String,
        remote_id: String,
        cert: Vec<u8>,
        name: String,
        address: String,
    ) -> Result<Self, String> {
        let record = Self {
            version: 1,
            transaction_id: tx,
            local_id,
            remote_id,
            remote_certificate: cert,
            remote_name: name,
            remote_address: address,
            mutually_confirmed: true,
            created_at: now()?,
            state: CommitState::Prepared,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), String> {
        record_path(Path::new("."), &self.transaction_id)?;
        if self.version != 1
            || !self.mutually_confirmed
            || self.local_id == self.remote_id
            || self.local_id.len() != 64
            || !self.local_id.bytes().all(|b| b.is_ascii_hexdigit())
            || certificate_id(&self.remote_certificate) != self.remote_id
            || self.remote_certificate.is_empty()
            || self.remote_certificate.len() > 4096
            || self.remote_name.is_empty()
            || self.remote_name.chars().count() > 63
            || self.remote_name.chars().any(char::is_control)
            || resolve_address(&self.remote_address).is_err()
            || now()?.saturating_sub(self.created_at) > RETENTION_SECS
            || self.created_at > now()?.saturating_add(300)
        {
            return Err("invalid or expired pairing recovery record".into());
        }
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let _lock = edit_lock(dir)?;
        self.save_locked(dir)
    }

    /// Caller holds `trust::edit_lock`, including across the associated trust write.
    pub(crate) fn save_locked(&self, dir: &Path) -> Result<(), String> {
        self.validate()?;
        let path = record_path(dir, &self.transaction_id)?;
        let folder = path.parent().ok_or("pairing record has no parent")?;
        crate::storage::directory(folder).map_err(|e| e.to_string())?;
        if path.exists() {
            let old = Self::load(dir, &self.transaction_id)?;
            if old.local_id != self.local_id
                || old.remote_id != self.remote_id
                || old.remote_certificate != self.remote_certificate
                || old.remote_name != self.remote_name
                || old.created_at != self.created_at
            {
                return Err("pairing transaction conflicts with existing recovery record".into());
            }
            if (self.state as u8) < (old.state as u8) {
                return Err("pairing state cannot move backwards".into());
            }
        } else if self.state != CommitState::Prepared {
            return Err("pairing recovery record was removed; refusing to recreate it".into());
        }
        atomic_write(
            &path,
            toml::to_string(self).map_err(|e| e.to_string())?.as_bytes(),
        )
    }

    pub fn load(dir: &Path, tx: &str) -> Result<Self, String> {
        let source = crate::storage::text(&record_path(dir, tx)?, 64 * 1024)
            .map_err(|e| format!("pairing recovery record: {e}"))?;
        let record: Self =
            toml::from_str(&source).map_err(|e| format!("invalid pairing recovery record: {e}"))?;
        record.validate()?;
        if record.transaction_id != tx {
            return Err("pairing transaction filename mismatch".into());
        }
        Ok(record)
    }

    pub fn remove(&self, dir: &Path) -> Result<(), String> {
        let _lock = edit_lock(dir)?;
        let path = record_path(dir, &self.transaction_id)?;
        crate::storage::remove(&path).map_err(|e| e.to_string())?;
        #[cfg(not(target_os = "windows"))]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)
                .and_then(|folder| folder.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn local_peer(&self, dir: &Path) -> Result<Option<Peer>, String> {
        let store = TrustStore::load(dir)?;
        let Some(peer) = store.find_id(&self.remote_id) else {
            if store.find(&self.remote_name).is_some() {
                return Err("pairing alias belongs to a different identity".into());
            }
            return Ok(None);
        };
        let saved = fs::read(cert_path(dir, &self.remote_id)).map_err(|e| e.to_string())?;
        if saved != self.remote_certificate {
            return Err("pairing certificate conflicts with trust store".into());
        }
        Ok(Some(peer.clone()))
    }

    pub fn status(&self, dir: &Path) -> LocalStatus {
        if self.validate().is_err() {
            return LocalStatus::Expired;
        }
        match self.local_peer(dir) {
            Err(_) => LocalStatus::Conflict,
            Ok(None) => LocalStatus::Prepared,
            Ok(Some(_)) if self.state == CommitState::Complete => LocalStatus::Complete,
            Ok(Some(_)) => LocalStatus::CommittedUnconfirmed,
        }
    }
}

pub fn list(dir: &Path) -> Result<Vec<PendingPairing>, String> {
    let folder = dir.join("pairing");
    #[cfg(target_os = "windows")]
    let _directory = match ukvm_platform::windows::state::StateDirectory::open(&folder, false) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("toml") {
            continue;
        }
        let source = crate::storage::text(&path, 64 * 1024).map_err(|e| e.to_string())?;
        #[cfg(target_os = "windows")]
        if source == ukvm_platform::windows::state::REVOKED_RECORD {
            continue;
        }
        let record: PendingPairing =
            toml::from_str(&source).map_err(|e| format!("invalid pairing recovery record: {e}"))?;
        if Some(format!("{}.toml", record.transaction_id).as_str())
            != path.file_name().and_then(|v| v.to_str())
        {
            return Err("pairing transaction filename mismatch".into());
        }
        records.push(record);
    }
    records.sort_by_key(|r| r.created_at);
    Ok(records)
}

pub fn cleanup_expired(dir: &Path) -> Result<usize, String> {
    let _lock = edit_lock(dir)?;
    let mut count = 0;
    for record in list(dir)? {
        if record.validate().is_err() {
            crate::storage::remove(&record_path(dir, &record.transaction_id)?)
                .map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    #[cfg(not(target_os = "windows"))]
    if count > 0 {
        fs::File::open(dir.join("pairing"))
            .and_then(|folder| folder.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(count)
}

/// Called by explicit peer removal while the caller owns the trust edit lock.
/// A removed peer must never be resurrected by an old recovery receipt.
pub(crate) fn revoke_for_peer(dir: &Path, peer_id: &str) -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    let mut removed = false;
    for record in list(dir)? {
        if record.remote_id == peer_id {
            crate::storage::remove(&record_path(dir, &record.transaction_id)?)
                .map_err(|e| e.to_string())?;
            #[cfg(not(target_os = "windows"))]
            {
                removed = true;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    if removed {
        fs::File::open(dir.join("pairing"))
            .and_then(|folder| folder.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn a_flushed_revocation_marker_never_recreates_trust_or_recovery() {
        let (temp, record, _) = fixture();
        let dir = temp.path().join("trust");
        record.save(&dir).unwrap();
        let path = record_path(&dir, &record.transaction_id).unwrap();
        atomic_write(
            &path,
            ukvm_platform::windows::state::REVOKED_RECORD.as_bytes(),
        )
        .unwrap();
        assert!(PendingPairing::load(&dir, &record.transaction_id).is_err());
        assert!(record.save(&dir).is_err());
        assert!(list(&dir).unwrap().is_empty());
        assert!(TrustStore::load(&dir).unwrap().peers.is_empty());
    }
    use crate::{identity::Identity, trust::SessionPermission};

    fn fixture() -> (tempfile::TempDir, PendingPairing, Identity) {
        let temp = tempfile::tempdir().unwrap();
        let local = Identity::load_or_create(&temp.path().join("local")).unwrap();
        let remote = Identity::load_or_create(&temp.path().join("remote")).unwrap();
        let record = PendingPairing::new(
            "a".repeat(64),
            local.id,
            remote.id.clone(),
            remote.tls_cert().as_ref().to_vec(),
            "io".into(),
            "127.0.0.1:24800".into(),
        )
        .unwrap();
        (temp, record, remote)
    }

    #[test]
    fn prepared_record_survives_reload_and_grants_no_trust() {
        let (temp, record, remote) = fixture();
        let dir = temp.path().join("trust");
        record.save(&dir).unwrap();
        let loaded = PendingPairing::load(&dir, &record.transaction_id).unwrap();
        assert_eq!(loaded.status(&dir), LocalStatus::Prepared);
        assert!(TrustStore::load(&dir)
            .unwrap()
            .find_id(&remote.id)
            .is_none());
        assert_eq!(fs::read_dir(dir.join("pairing")).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(record_path(&dir, &record.transaction_id).unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.join("pairing"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn exact_peer_is_idempotent_and_permissions_are_unchanged() {
        let (temp, mut record, remote) = fixture();
        let dir = temp.path().join("trust");
        record.save(&dir).unwrap();
        let mut store = TrustStore::default();
        store
            .import(
                &dir,
                "chosen-alias".into(),
                Some("10.0.0.9".into()),
                remote.tls_cert().as_ref(),
            )
            .unwrap();
        store
            .set_permission(&dir, "chosen-alias", SessionPermission::DebugInput, true)
            .unwrap();
        assert_eq!(record.status(&dir), LocalStatus::CommittedUnconfirmed);
        record.state = CommitState::CommittedLocally;
        record.save(&dir).unwrap();
        record.state = CommitState::Complete;
        record.save(&dir).unwrap();
        assert!(record.save(&dir).is_ok());
        assert_eq!(record.status(&dir), LocalStatus::Complete);
        let peer = record.local_peer(&dir).unwrap().unwrap();
        assert_eq!(peer.alias, "chosen-alias");
        assert_eq!(peer.address.as_deref(), Some("10.0.0.9"));
        assert!(peer.permissions.debug_input);
        assert!(record.state == CommitState::Complete);
        let mut backwards = record.clone();
        backwards.state = CommitState::Prepared;
        assert!(backwards.save(&dir).is_err());
    }

    #[test]
    fn mismatch_and_expiry_fail_closed() {
        let (temp, record, remote) = fixture();
        let dir = temp.path().join("trust");
        record.save(&dir).unwrap();
        let mut wrong_tx = record.clone();
        wrong_tx.remote_id = "b".repeat(64);
        assert!(wrong_tx.save(&dir).is_err());
        let mut wrong_cert = record.clone();
        wrong_cert.remote_certificate[0] ^= 1;
        assert!(wrong_cert.save(&dir).is_err());
        assert!(PendingPairing::load(&dir, &"b".repeat(64)).is_err());
        let other = Identity::load_or_create(&temp.path().join("other")).unwrap();
        TrustStore::default()
            .import(&dir, "io".into(), None, other.tls_cert().as_ref())
            .unwrap();
        assert_eq!(record.status(&dir), LocalStatus::Conflict);
        assert!(TrustStore::load(&dir)
            .unwrap()
            .find_id(&remote.id)
            .is_none());
        let mut expired = record.clone();
        expired.created_at = now().unwrap() - RETENTION_SECS - 1;
        let path = record_path(&dir, &record.transaction_id).unwrap();
        atomic_write(&path, toml::to_string(&expired).unwrap().as_bytes()).unwrap();
        assert_eq!(list(&dir).unwrap()[0].status(&dir), LocalStatus::Expired);
        assert!(PendingPairing::load(&dir, &record.transaction_id).is_err());
        assert_eq!(cleanup_expired(&dir).unwrap(), 1);
        assert!(list(&dir).unwrap().is_empty());
        assert_eq!(TrustStore::load(&dir).unwrap().peers.len(), 1);
    }

    #[test]
    fn explicit_peer_removal_revokes_recovery_receipt() {
        let (temp, record, remote) = fixture();
        let dir = temp.path().join("trust");
        record.save(&dir).unwrap();
        let mut store = TrustStore::default();
        store
            .import(&dir, "io".into(), None, remote.tls_cert().as_ref())
            .unwrap();
        store.remove(&dir, "io").unwrap();
        assert!(list(&dir).unwrap().is_empty());
        assert!(PendingPairing::load(&dir, &record.transaction_id).is_err());
    }
}
