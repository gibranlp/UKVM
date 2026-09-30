//! Setup decisions without terminal I/O. Discovery fields remain untrusted
//! until a pinned TLS connection or Pairing Protocol 2 proves identity.
use std::path::{Path, PathBuf};
use ukvm_config::{Config, Edge};
use ukvm_discovery::DiscoveryResult;
use ukvm_network::{
    pairing_state::PendingPairing,
    trust::{Peer, TrustStore},
};

#[derive(Clone, Debug)]
pub enum CandidateStatus {
    SelfClaim,
    KnownHint(Peer),
    AliasConflict {
        trusted: Peer,
        claimed_id: Option<String>,
    },
    New,
}

pub fn classify(
    candidate: &DiscoveryResult,
    trust: &TrustStore,
    local_id: &str,
) -> CandidateStatus {
    let claimed = candidate.advertised_node_id.as_deref();
    if claimed == Some(local_id) {
        return CandidateStatus::SelfClaim;
    }
    if let Some(named) = trust
        .peers
        .iter()
        .find(|peer| peer.alias == candidate.display_name)
    {
        if claimed != Some(named.id.as_str()) {
            return CandidateStatus::AliasConflict {
                trusted: named.clone(),
                claimed_id: candidate.advertised_node_id.clone(),
            };
        }
    }
    if let Some(peer) = claimed.and_then(|id| trust.find_id(id)) {
        return CandidateStatus::KnownHint(peer.clone());
    }
    CandidateStatus::New
}

pub fn matching_recovery<'a>(
    records: &'a [PendingPairing],
    candidate: &DiscoveryResult,
) -> Vec<&'a PendingPairing> {
    records
        .iter()
        .filter(|record| {
            candidate.advertised_node_id.as_deref() == Some(record.remote_id.as_str())
                || record.remote_name == candidate.display_name
        })
        .collect()
}

pub fn temporary_paths(paths: &[&Path]) -> Vec<PathBuf> {
    let temp = std::env::temp_dir();
    paths
        .iter()
        .filter(|path| path.starts_with(&temp))
        .map(|path| (*path).to_path_buf())
        .collect()
}

pub fn current_edge(config: &Config, peer_id: &str) -> Option<Edge> {
    config
        .layout
        .links
        .iter()
        .find(|link| link.peer_id == peer_id)
        .map(|link| link.edge)
}

/// Reuse the existing node-level layout. `replace` only permits replacing a
/// different peer already assigned to this edge after explicit confirmation.
pub fn set_peer_edge(
    config: &mut Config,
    peer_id: &str,
    edge: Edge,
    replace: bool,
) -> Result<bool, String> {
    if let Some(occupied) = config
        .layout
        .links
        .iter()
        .find(|link| link.edge == edge && link.peer_id != peer_id)
    {
        if !replace {
            return Err(format!(
                "{} edge already belongs to peer {}",
                edge.as_str(),
                occupied.peer_id
            ));
        }
    }
    if config
        .layout
        .links
        .iter()
        .filter(|link| link.peer_id == peer_id)
        .count()
        == 1
        && current_edge(config, peer_id) == Some(edge)
    {
        return Ok(false);
    }
    config.layout.links.retain(|link| link.peer_id != peer_id);
    config.set_link(edge, peer_id.to_string());
    config.validate().map_err(|e| e.to_string())?;
    Ok(true)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilitySummary {
    pub capture: String,
    pub injection: String,
}

pub fn capability_summary(
    capture_advertised: bool,
    injection_implemented: bool,
    reason: &str,
) -> CapabilitySummary {
    CapabilitySummary {
        capture: if capture_advertised {
            "portal advertised; live permission not checked"
        } else {
            "unavailable or not detected"
        }
        .into(),
        injection: if injection_implemented {
            "backend implemented; authorization and live proof still required".into()
        } else {
            format!("unavailable: {reason}")
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukvm_network::{identity::Identity, pairing_state::PendingPairing, trust::Permissions};

    fn peer(id: &str, alias: &str) -> Peer {
        Peer {
            id: id.into(),
            alias: alias.into(),
            address: Some("192.168.1.40:24800".into()),
            permissions: Permissions {
                connect: true,
                debug_input: false,
                control_this_node: false,
            },
        }
    }
    fn candidate(name: &str, id: Option<&str>) -> DiscoveryResult {
        DiscoveryResult {
            service_instance: "test._ukvm._tcp.local.".into(),
            display_name: name.into(),
            advertised_node_id: id.map(str::to_string),
            pairing_version: 2,
            pairing_available: true,
            input_version: Some((1, 1)),
            control_port: Some(24800),
            addresses: vec!["192.168.1.40:24801".parse().unwrap()],
        }
    }

    #[test]
    fn known_identity_and_name_conflict_are_distinct() {
        let store = TrustStore {
            peers: vec![peer(&"a".repeat(64), "io")],
        };
        assert!(matches!(
            classify(
                &candidate("io", Some(&"a".repeat(64))),
                &store,
                &"b".repeat(64)
            ),
            CandidateStatus::KnownHint(_)
        ));
        assert!(matches!(
            classify(
                &candidate("io", Some(&"c".repeat(64))),
                &store,
                &"b".repeat(64)
            ),
            CandidateStatus::AliasConflict { .. }
        ));
        assert!(matches!(
            classify(&candidate("io", None), &store, &"b".repeat(64)),
            CandidateStatus::AliasConflict { .. }
        ));
        assert!(matches!(
            classify(
                &candidate("laptop", Some(&"c".repeat(64))),
                &store,
                &"b".repeat(64)
            ),
            CandidateStatus::New
        ));
        assert!(matches!(
            classify(
                &candidate("me", Some(&"b".repeat(64))),
                &store,
                &"b".repeat(64)
            ),
            CandidateStatus::SelfClaim
        ));
    }

    #[test]
    fn all_edges_preserve_or_move_existing_layout() {
        let id = "a".repeat(64);
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let mut config = Config::default();
            assert!(set_peer_edge(&mut config, &id, edge, false).unwrap());
            assert_eq!(current_edge(&config, &id), Some(edge));
            assert!(!set_peer_edge(&mut config, &id, edge, false).unwrap());
            assert_eq!(config.layout.links.len(), 1);
        }
        let mut config = Config::default();
        config.set_link(Edge::Right, id.clone());
        assert!(set_peer_edge(&mut config, &id, Edge::Left, false).unwrap());
        assert_eq!(config.layout.links.len(), 1);
        config.set_link(Edge::Top, "b".repeat(64));
        assert!(set_peer_edge(&mut config, &id, Edge::Top, false).is_err());
        assert!(set_peer_edge(&mut config, &id, Edge::Top, true).unwrap());
        assert_eq!(config.layout.links.len(), 1);
    }

    #[test]
    fn temporary_path_and_unsupported_capability_reporting() {
        let temp = std::env::temp_dir().join("ukvm-profile/node.der");
        let ordinary = Path::new("/home/user/.local/share/ukvm/node.der");
        assert_eq!(temporary_paths(&[&temp, ordinary]), vec![temp]);
        let summary = capability_summary(true, false, "RemoteDesktop portal unavailable");
        assert!(summary.capture.contains("advertised"));
        assert!(summary.injection.contains("unavailable"));
    }

    #[test]
    fn setup_reuses_identity_and_recovery_is_only_a_hint() {
        let temp = tempfile::tempdir().unwrap();
        let local = Identity::load_or_create(&temp.path().join("local")).unwrap();
        let remote = Identity::load_or_create(&temp.path().join("remote")).unwrap();
        let again = Identity::load_or_create(&temp.path().join("local")).unwrap();
        assert_eq!(local.id, again.id);
        let record = PendingPairing::new(
            "c".repeat(64),
            local.id,
            remote.id.clone(),
            remote.tls_cert().as_ref().to_vec(),
            "io".into(),
            "192.168.1.40:24800".into(),
        )
        .unwrap();
        let records = [record];
        let found = matching_recovery(&records, &candidate("io", Some(&remote.id)));
        assert_eq!(found.len(), 1);
        // Discovery/recovery classification does not write a trust record.
        assert!(TrustStore::load(&temp.path().join("trust"))
            .unwrap()
            .peers
            .is_empty());
    }

    #[test]
    fn classification_does_not_grant_input_permissions() {
        let id = "a".repeat(64);
        let store = TrustStore {
            peers: vec![peer(&id, "io")],
        };
        let result = classify(&candidate("io", Some(&id)), &store, &"b".repeat(64));
        assert!(matches!(result, CandidateStatus::KnownHint(_)));
        let peer = store.find_id(&id).unwrap();
        assert!(!peer.permissions.debug_input);
        assert!(!peer.permissions.control_this_node);
    }
}
