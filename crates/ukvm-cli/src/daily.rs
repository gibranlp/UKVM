//! Read-only daily views. Files/discovery hints do not grant trust or authority.
use std::{collections::HashMap, process::ExitCode};
use ukvm_network::{
    identity::{certificate_id, default_state_dir},
    trust::{default_trust_dir, TrustStore},
};
#[derive(Default)]
struct Live {
    runtime: String,
    name: String,
    node_id: String,
    focus: String,
    peer: Option<String>,
    coordinate: Option<String>,
    capture: String,
    receiver: String,
    states: HashMap<String, String>,
    clipboard: String,
}
fn live() -> Live {
    #[cfg(target_os = "linux")]
    {
        use ukvm_runtime::ownership::{status, RuntimeStatus};
        match status() {
            Ok(RuntimeStatus::Stopped) => Live {
                runtime: "stopped (no runtime lock owner)".into(),
                ..Default::default()
            },
            Ok(RuntimeStatus::StartingOrUnavailable(error)) => Live {
                runtime: format!("owned, live state unknown: {error}"),
                ..Default::default()
            },
            Err(error) => Live {
                runtime: format!("unknown: {error}"),
                ..Default::default()
            },
            Ok(RuntimeStatus::Running(snapshot)) => Live {
                runtime: format!(
                    "{} (PID {}, sampled within 5 seconds)",
                    snapshot.state, snapshot.pid
                ),
                name: snapshot.name,
                node_id: snapshot.node_id,
                focus: snapshot.focus,
                peer: snapshot.active_peer,
                coordinate: snapshot.active_coordinate,
                capture: snapshot.capture,
                receiver: snapshot.receiver,
                clipboard: format!(
                    "state={} supported={:?} enabled={} connected-peers={} last-sync={:?}",
                    snapshot.clipboard.state,
                    snapshot.clipboard.supported,
                    snapshot.clipboard.enabled,
                    snapshot.clipboard.connected_peers,
                    snapshot.clipboard.last_sync
                ),
                states: snapshot
                    .peers
                    .into_iter()
                    .map(|peer| (peer.id, peer.availability))
                    .collect(),
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    Live {
        runtime: "runtime/service unsupported on this OS".into(),
        ..Default::default()
    }
}
fn peer_lines(store: &TrustStore, live: &Live) -> String {
    if store.peers.is_empty() {
        return "No trusted peers.\n".into();
    }
    store.peers.iter().map(|peer| format!("{}  pinned={}  connect={} debug-input={} control-this-node={} share-clipboard={} receive-files={} view-screen={}  availability={}  address={}\n",
        peer.alias, peer.id, peer.permissions.connect, peer.permissions.debug_input, peer.permissions.control_this_node, peer.permissions.share_clipboard, peer.permissions.receive_files, peer.permissions.view_screen,
        live.states.get(&peer.id).map(String::as_str).unwrap_or("unknown (no fresh configured runtime observation)"),
        peer.address.as_deref().unwrap_or("none"))).collect()
}
fn context() -> Result<(ukvm_config::Config, TrustStore), String> {
    let config =
        ukvm_config::load_or_default(&ukvm_config::default_path().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let trust = TrustStore::load(&default_trust_dir()?)?;
    Ok((config, trust))
}
pub fn peers(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return super::fail("usage: ukvm peers");
    }
    match context() {
        Ok((config, store)) => {
            print!(
                "Trusted peers (local permissions; readiness never grants authorization)\n{}",
                peer_lines(&store, &live())
            );
            for node in config.layout.topology() {
                println!(
                    "Position {}: {} [{}]",
                    node.coordinate(),
                    store
                        .find_id(&node.peer_id)
                        .map(|p| p.alias.as_str())
                        .unwrap_or("UNTRUSTED"),
                    node.peer_id
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => super::fail(&error),
    }
}
pub fn status() -> ExitCode {
    super::authorization::print_status(&super::authorization::status(None, None));
    let result = (|| -> Result<(), String> {
        let (config, store) = context()?;
        let id = match std::fs::read(default_state_dir()?.join("node.der")) {
            Ok(bytes) => certificate_id(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                "not initialized; run ukvm identity".into()
            }
            Err(error) => return Err(error.to_string()),
        };
        let live = live();
        let id = if live.node_id.is_empty() {
            &id
        } else {
            &live.node_id
        };
        println!(
            "UniversalKVM\nNode: {}\nPinned node ID: {id}\nRuntime: {}\nService: {}",
            if live.name.is_empty() {
                &config.node.name
            } else {
                &live.name
            },
            live.runtime,
            service_summary()
        );
        println!(
            "Focus: {}\nActive peer: {}\nActive coordinate: {}",
            if live.focus.is_empty() {
                "unknown (runtime not reporting)"
            } else {
                &live.focus
            },
            live.peer.as_deref().unwrap_or("none/unknown"),
            live.coordinate
                .as_deref()
                .unwrap_or("local/inbound/unknown")
        );
        println!(
            "Clipboard: {}",
            if live.clipboard.is_empty() {
                "runtime not reporting; see ukvm status --json".to_string()
            } else {
                live.clipboard.clone()
            }
        );
        println!(
            "Configured topology (loaded by runtime at startup):\n  (0,0): {} (local)",
            config.node.name
        );
        for node in config.layout.topology() {
            let alias = store
                .find_id(&node.peer_id)
                .map(|peer| peer.alias.as_str())
                .unwrap_or("UNKNOWN TRUST RECORD; unavailable");
            println!(
                "  {}: {alias} [{}], availability={}",
                node.coordinate(),
                node.peer_id,
                live.states
                    .get(&node.peer_id)
                    .map(String::as_str)
                    .unwrap_or("unknown")
            );
        }
        print!("Trusted peers\n{}", peer_lines(&store, &live));
        if !live.capture.is_empty() || !live.receiver.is_empty() {
            println!(
                "Runtime backend\n  Capture: {}\n  Receiver: {}",
                live.capture, live.receiver
            );
        } else {
            #[cfg(target_os = "linux")]
            {
                let probe = super::with_runtime(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        ukvm_platform::linux_wayland::probe(),
                    )
                    .await
                });
                match probe {
                    Ok(probe) => println!("Platform (current terminal, capability probe only; no consent requested)\n  InputCapture: v{:?}, pointer={}, keyboard={}\n  RemoteDesktop: v{:?}, pointer={}\n  Backend readiness: unknown until runtime obtains normal portal/EI consent", probe.capture.version, probe.capture.pointer_advertised, probe.capture.keyboard_advertised, probe.remote.version, probe.remote.pointer_advertised),
                    Err(_) => println!("Platform: portal capability probe timed out; backend readiness unknown"),
                }
            }
        }
        println!("Known limitation: Hyprland partial external-edge barriers may be rejected; see docs/TOPOLOGY.md.\nDiscovery/availability is advisory; pinned authentication and explicit permission are still required.");
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => super::fail(&error),
    }
}
fn service_summary() -> String {
    #[cfg(target_os = "linux")]
    {
        super::service::summary()
    }
    #[cfg(not(target_os = "linux"))]
    {
        "unsupported on this OS".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peers_include_pins_authorization_and_unknown_or_live_availability() {
        use ukvm_network::trust::{Peer, Permissions};
        let mut store = TrustStore::default();
        store.peers.push(Peer {
            id: "a".repeat(64),
            alias: "io".into(),
            address: None,
            permissions: Permissions {
                connect: true,
                share_clipboard: false,
                receive_files: false,
                view_screen: false,
                debug_input: false,
                control_this_node: true,
            },
        });
        let mut live = Live::default();
        let text = peer_lines(&store, &live);
        assert!(text.contains(&format!("pinned={}", "a".repeat(64))));
        assert!(text.contains("connect=true debug-input=false control-this-node=true"));
        assert!(text.contains("availability=unknown"));
        live.states.insert("a".repeat(64), "Ready".into());
        assert!(peer_lines(&store, &live).contains("availability=Ready"));
    }
}
