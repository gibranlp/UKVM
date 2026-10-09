//! User-owned peer trust records, separate from preferences and the node private key.
use crate::identity::certificate_id;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Trust decisions are mutable user state, separate from the long-lived TLS identity.
pub fn default_trust_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .ok_or("unable to locate XDG user state directory")?;
        Ok(base.join("ukvm"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(crate::identity::default_state_dir()?.join("state"))
    }
}

/// Serialize read-modify-write trust operations across local processes.
pub fn edit_lock(dir: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(dir.join(".edit.lock"))
        .map_err(|e| e.to_string())?;
    fs2::FileExt::lock_exclusive(&file).map_err(|e| format!("cannot lock UKVM edits: {e}"))?;
    Ok(file)
}

pub fn clipboard_guard(dir: &Path, id: &str) -> Result<fs::File, String> {
    permission_guard(dir, id, SessionPermission::ShareClipboard)
}

pub fn permission_guard(
    dir: &Path,
    id: &str,
    permission: SessionPermission,
) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let guard = options
        .open(dir.join(".edit.lock"))
        .map_err(|_| "clipboard policy unavailable".to_string())?;
    fs2::FileExt::try_lock_shared(&guard)
        .map_err(|_| "clipboard policy edit in progress".to_string())?;
    TrustStore::load(dir)?.authorize(id, permission)?;
    Ok(guard)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub connect: bool,
    #[serde(default)]
    pub debug_input: bool,
    #[serde(default)]
    pub control_this_node: bool,
    #[serde(default)]
    pub share_clipboard: bool,
    #[serde(default)]
    pub receive_files: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub id: String,
    pub alias: String,
    pub address: Option<String>,
    #[serde(default)]
    pub permissions: Permissions,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    #[serde(default)]
    pub peers: Vec<Peer>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPermission {
    Connect,
    DebugInput,
    ControlThisNode,
    ShareClipboard,
    ReceiveFiles,
}

impl TrustStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("peers.toml");
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let store: Self =
            toml::from_str(&source).map_err(|e| format!("invalid peer store: {e}"))?;
        store.validate(dir)?;
        Ok(store)
    }
    pub fn validate(&self, dir: &Path) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        let mut aliases = std::collections::HashSet::new();
        for peer in &self.peers {
            if peer.id.len() != 64 || !peer.id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("invalid peer ID for {}", peer.alias));
            }
            if peer.alias.is_empty()
                || peer.alias.chars().count() > 63
                || peer.alias.chars().any(char::is_control)
            {
                return Err(format!("invalid peer alias: {}", peer.alias));
            }
            if !ids.insert(&peer.id) || !aliases.insert(&peer.alias) {
                return Err(format!("duplicate peer ID or alias: {}", peer.alias));
            }
            if let Some(address) = &peer.address {
                resolve_address(address)?;
            }
            let cert = fs::read(cert_path(dir, &peer.id))
                .map_err(|e| format!("certificate for {}: {e}", peer.alias))?;
            if cert.is_empty() || cert.len() > 4096 || certificate_id(&cert) != peer.id {
                return Err(format!(
                    "certificate fingerprint mismatch for {}",
                    peer.alias
                ));
            }
            validate_certificate(&cert)?;
        }
        Ok(())
    }
    pub fn find(&self, name: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.alias == name || p.id == name)
    }
    pub fn find_id(&self, id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.id == id)
    }
    pub fn authorize(&self, id: &str, permission: SessionPermission) -> Result<&Peer, String> {
        let peer = self
            .find_id(id)
            .ok_or("authenticated peer is not trusted")?;
        let allowed = match permission {
            SessionPermission::ReceiveFiles => {
                peer.permissions.connect && peer.permissions.receive_files
            }
            SessionPermission::ShareClipboard => {
                peer.permissions.connect && peer.permissions.share_clipboard
            }
            SessionPermission::Connect => peer.permissions.connect,
            SessionPermission::DebugInput => {
                peer.permissions.connect && peer.permissions.debug_input
            }
            SessionPermission::ControlThisNode => {
                peer.permissions.connect && peer.permissions.control_this_node
            }
        };
        if allowed {
            Ok(peer)
        } else {
            Err(format!("{} is denied {:?}", peer.alias, permission))
        }
    }
    pub fn import(
        &mut self,
        dir: &Path,
        alias: String,
        address: Option<String>,
        cert: &[u8],
    ) -> Result<Peer, String> {
        if cert.is_empty() || cert.len() > 4096 {
            return Err("certificate length is invalid".into());
        }
        validate_certificate(cert)?;
        if let Some(a) = &address {
            resolve_address(a)?;
        }
        let peer = Peer {
            id: certificate_id(cert),
            alias,
            address,
            permissions: Permissions {
                connect: true,
                ..Permissions::default()
            },
        };
        if self.find(&peer.id).is_some() || self.find(&peer.alias).is_some() {
            return Err("peer ID or alias already exists".into());
        }
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        private_dir(dir)?;
        atomic_write(&cert_path(dir, &peer.id), cert)?;
        self.peers.push(peer.clone());
        if let Err(e) = self.save(dir) {
            self.peers.pop();
            let _ = fs::remove_file(cert_path(dir, &peer.id));
            return Err(e);
        }
        Ok(peer)
    }
    pub fn remove(&mut self, dir: &Path, name: &str) -> Result<Peer, String> {
        let index = self
            .peers
            .iter()
            .position(|p| p.alias == name || p.id == name)
            .ok_or("unknown peer")?;
        let peer = self.peers[index].clone();
        // An explicit removal revokes any still-retained pairing receipt, so
        // recovery cannot silently recreate this trust decision.
        crate::pairing_state::revoke_for_peer(dir, &peer.id)?;
        let peer = self.peers.remove(index);
        if let Err(e) = self.save(dir) {
            self.peers.insert(index, peer);
            return Err(e);
        }
        fs::remove_file(cert_path(dir, &peer.id)).map_err(|e| e.to_string())?;
        Ok(peer)
    }
    pub fn set_permission(
        &mut self,
        dir: &Path,
        name: &str,
        permission: SessionPermission,
        allowed: bool,
    ) -> Result<(), String> {
        let peer = self
            .peers
            .iter_mut()
            .find(|p| p.alias == name || p.id == name)
            .ok_or("unknown peer")?;
        match permission {
            SessionPermission::ShareClipboard => peer.permissions.share_clipboard = allowed,
            SessionPermission::ReceiveFiles => peer.permissions.receive_files = allowed,
            SessionPermission::Connect => peer.permissions.connect = allowed,
            SessionPermission::DebugInput => peer.permissions.debug_input = allowed,
            SessionPermission::ControlThisNode => peer.permissions.control_this_node = allowed,
        }
        self.save(dir)
    }
    /// Update an authenticated peer's connection hint without changing its
    /// certificate, alias, permissions, or layout. Caller must verify the new
    /// endpoint against the pinned certificate before invoking this method.
    pub fn set_address(&mut self, dir: &Path, id: &str, address: &str) -> Result<(), String> {
        let address = resolve_address(address)?;
        let peer = self.find_id(id).ok_or("unknown peer")?;
        if peer.address.as_deref() == Some(address.as_str()) {
            return Ok(());
        }
        let peer = self
            .peers
            .iter_mut()
            .find(|peer| peer.id == id)
            .ok_or("unknown peer")?;
        let old = peer.address.replace(address);
        if let Err(error) = self.save(dir) {
            self.peers
                .iter_mut()
                .find(|peer| peer.id == id)
                .ok_or("unknown peer")?
                .address = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        private_dir(dir)?;
        self.validate(dir)?;
        let mut value = toml::Value::try_from(self).map_err(|e| e.to_string())?;
        // El valor falso se omite para que una actualización sin permisos nuevos siga siendo legible por M19.
        if let Some(peers) = value.get_mut("peers").and_then(toml::Value::as_array_mut) {
            for peer in peers {
                if let Some(permissions) = peer
                    .get_mut("permissions")
                    .and_then(toml::Value::as_table_mut)
                {
                    if permissions.get("receive_files") == Some(&toml::Value::Boolean(false)) {
                        permissions.remove("receive_files");
                    }
                }
            }
        }
        atomic_write(
            &dir.join("peers.toml"),
            toml::to_string_pretty(&value)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
    }
}

pub fn cert_path(dir: &Path, id: &str) -> PathBuf {
    dir.join("peers").join(format!("{id}.der"))
}
pub fn resolve_address(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '/')
    {
        return Err("invalid peer address".into());
    }
    let result = if value.starts_with('[') {
        if value.contains("]:") {
            value.into()
        } else if value.ends_with(']') {
            format!("{value}:{}", crate::DEFAULT_PORT)
        } else {
            return Err("invalid IPv6 address".into());
        }
    } else if value.matches(':').count() == 0 {
        format!("{value}:{}", crate::DEFAULT_PORT)
    } else if value.matches(':').count() == 1 {
        value.into()
    } else {
        return Err("IPv6 addresses must use [address]:port".into());
    };
    let (host, port) = result.rsplit_once(':').ok_or("invalid address")?;
    if host.is_empty() || port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
        return Err("peer address requires a valid host and port".into());
    }
    Ok(result)
}
fn validate_certificate(cert: &[u8]) -> Result<(), String> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(cert.to_vec()))
        .map_err(|e| format!("invalid X.509 certificate: {e}"))
}
fn private_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir.join("peers")).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(dir.join("peers"), fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(
        ".ukvm-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| -> Result<(), String> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temp).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    #[test]
    fn import_and_permission_separation() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let cert = fs::read(identity.cert_path).unwrap();
        let dir = temp.path().join("trust");
        let mut store = TrustStore::default();
        let peer = store
            .import(&dir, "io".into(), Some("192.168.1.40".into()), &cert)
            .unwrap();
        assert_eq!(peer.id, certificate_id(&cert));
        assert!(store
            .authorize(&peer.id, SessionPermission::Connect)
            .is_ok());
        assert!(store
            .authorize(&peer.id, SessionPermission::DebugInput)
            .is_err());
        assert!(store
            .authorize(&peer.id, SessionPermission::ControlThisNode)
            .is_err());
        assert!(store.import(&dir, "again".into(), None, &cert).is_err());
        store
            .set_permission(&dir, "io", SessionPermission::DebugInput, true)
            .unwrap();
        assert!(TrustStore::load(&dir)
            .unwrap()
            .authorize(&peer.id, SessionPermission::DebugInput)
            .is_ok());
        store.remove(&dir, "io").unwrap();
        assert!(TrustStore::load(&dir).unwrap().peers.is_empty());
    }

    #[test]
    fn address_hint_update_preserves_identity_and_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(&temp.path().join("identity")).unwrap();
        let dir = temp.path().join("trust");
        let mut store = TrustStore::default();
        let peer = store
            .import(
                &dir,
                "io".into(),
                Some("192.168.1.40".into()),
                identity.tls_cert().as_ref(),
            )
            .unwrap();
        store
            .set_permission(&dir, &peer.id, SessionPermission::DebugInput, true)
            .unwrap();
        store.set_address(&dir, &peer.id, "192.168.1.55").unwrap();
        store.set_address(&dir, &peer.id, "192.168.1.55").unwrap();
        let restored = TrustStore::load(&dir).unwrap();
        let updated = restored.find_id(&peer.id).unwrap();
        assert_eq!(updated.alias, "io");
        assert_eq!(updated.address.as_deref(), Some("192.168.1.55:24800"));
        assert!(updated.permissions.debug_input);
        assert!(!updated.permissions.control_this_node);
    }
    #[test]
    fn default_port() {
        assert_eq!(
            resolve_address("192.168.1.40").unwrap(),
            "192.168.1.40:24800"
        );
        assert_eq!(
            resolve_address("example.local:1234").unwrap(),
            "example.local:1234"
        );
    }
}
