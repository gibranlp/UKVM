//! Terminal presentation for the existing identity, discovery, pairing, trust
//! and layout operations. Security decisions live in `setup_model` and the
//! existing network/config crates, not in discovery TXT records.
use crate::{
    pairing,
    setup_model::{self, CandidateStatus},
};
use std::{
    io::{self, IsTerminal, Write},
    net::SocketAddr,
    path::Path,
    process::ExitCode,
    time::Duration,
};
use ukvm_config::{Config, Edge};
use ukvm_discovery::{DiscoveryResult, DEFAULT_BROWSE};
use ukvm_network::{
    identity::{default_state_dir, Identity},
    pairing::{pair_stream, PairRole, PAIRING_TIMEOUT_SECS, PAIRING_VERSION},
    pairing_state::{self, LocalStatus},
    transport,
    trust::{cert_path, default_trust_dir, edit_lock, Peer, SessionPermission, TrustStore},
};

const HELP: &str = "Usage: ukvm setup [--interface NAME] [--host HOST[:PORT]] [--debug]\n\
Guide identity, platform readiness, user service, pairing, permissions, and layout.\n\
Discovery is untrusted; new trust requires Pairing Protocol 2 and code comparison on both computers.\n\
No --yes option exists. Run interactively in a terminal.";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Options {
    interface: Option<String>,
    host: Option<String>,
    debug: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--interface" if options.interface.is_none() => {
                let value = args.get(index + 1).ok_or("--interface requires a name")?;
                if value.is_empty() || value.starts_with('-') {
                    return Err("--interface requires a name".into());
                }
                options.interface = Some(value.clone());
                index += 2;
            }
            "--host" if options.host.is_none() => {
                let value = args
                    .get(index + 1)
                    .ok_or("--host requires a hostname or IP")?;
                pairing::target_address(value)?;
                options.host = Some(value.clone());
                index += 2;
            }
            "--debug" if !options.debug => {
                options.debug = true;
                index += 1;
            }
            _ => return Err(HELP.into()),
        }
    }
    Ok(options)
}

fn address_changed(current: Option<&str>, verified: &str) -> bool {
    current
        .and_then(|value| ukvm_network::trust::resolve_address(value).ok())
        .as_deref()
        != Some(verified)
}

fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    if io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?
        == 0
    {
        return Err("setup input closed; no further changes made".into());
    }
    Ok(answer.trim().to_string())
}

fn yes_no(prompt: &str, default_yes: bool) -> Result<bool, String> {
    let suffix = if default_yes { " [Y/n] " } else { " [y/N] " };
    let answer = ask(&format!("{prompt}{suffix}"))?;
    match answer.to_ascii_lowercase().as_str() {
        "" => Ok(default_yes),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err("answer yes or no; setup stopped without applying this step".into()),
    }
}

fn choice(prompt: &str, allowed: &[&str]) -> Result<String, String> {
    loop {
        let answer = ask(prompt)?;
        if allowed.contains(&answer.as_str()) {
            return Ok(answer);
        }
        println!("Choose one of: {}", allowed.join(", "));
    }
}

pub fn setup(args: &[String]) -> ExitCode {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(args) {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return super::fail("ukvm setup requires an interactive terminal for identity and pairing confirmation; use individual commands for scripted work");
    }
    match super::with_runtime(run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => super::fail(&error),
    }
}

async fn run(options: Options) -> Result<(), String> {
    println!("UniversalKVM Setup\n");
    let config_path = ukvm_config::default_path().map_err(|e| e.to_string())?;
    let identity_dir = default_state_dir()?;
    let trust_dir = default_trust_dir()?;
    let temporary = setup_model::temporary_paths(&[&config_path, &identity_dir, &trust_dir]);
    if !temporary.is_empty() {
        println!("Warning: this UKVM profile uses temporary paths:");
        for path in &temporary {
            println!("  {}", path.display());
        }
        println!("Its identity or configuration may disappear after cleanup.");
        if !yes_no("Continue with this temporary profile?", false)? {
            return Err("setup cancelled before identity creation".into());
        }
    }
    let identity = Identity::load_or_create(&identity_dir)?;
    let mut config = ukvm_config::load_or_default(&config_path).map_err(|e| e.to_string())?;
    if !config_path.exists() {
        ukvm_config::save(&config_path, &config).map_err(|e| e.to_string())?;
    }
    println!(
        "This computer:\n  Name: {}\n  Node ID: {}\n  Public certificate: {}",
        config.node.name,
        identity.id,
        identity.cert_path.display()
    );
    if options.debug {
        eprintln!(
            "setup identity ID={} config={} trust={}",
            identity.id,
            config_path.display(),
            trust_dir.display()
        );
    }
    let addresses = match ukvm_discovery::local_interface_addresses(options.interface.as_deref()) {
        Ok(addresses) => addresses,
        Err(error) => {
            println!("Discovery interface unavailable: {error}. Direct pairing remains available.");
            Vec::new()
        }
    };
    for address in &addresses {
        println!("  Network: {address}");
    }
    if addresses.is_empty() {
        println!("  Network: no eligible local interface found");
    }
    if yes_no("Change this computer's display name?", false)? {
        let name = ask("New display name: ")?;
        if name.is_empty() {
            return Err("display name cannot be empty".into());
        }
        let _lock = edit_lock(&trust_dir)?;
        config = ukvm_config::load_or_default(&config_path).map_err(|e| e.to_string())?;
        config.node.name = name;
        ukvm_config::save(&config_path, &config).map_err(|e| e.to_string())?;
    }
    let capability = platform_capability().await;
    println!(
        "\nPlatform readiness (no consent requested):\n  Capture: {}\n  Injection: {}",
        capability.capture, capability.injection
    );
    #[cfg(target_os = "linux")]
    {
        println!("\nUser service: {}", crate::service::summary());
        if yes_no(
            "Install/update the UKVM user service using this executable?",
            false,
        )? && crate::service::command(&["install".into()]) != ExitCode::SUCCESS
        {
            return Err(
                "service installation failed; identity and existing trust were retained".into(),
            );
        }
        if yes_no(
            "Enable UKVM automatically for future graphical logins?",
            false,
        )? {
            crate::service::systemctl(&["enable", "ukvm.service"])?;
        }
        if yes_no(
            "Start/restart the managed UKVM service now (normal portal consent may appear)?",
            false,
        )? {
            crate::management::apply_configuration()?;
        }
    }
    println!("\nDiscovery readiness: {} eligible address(es). Pairing uses temporary port 24801 and requires comparing codes on both computers. A running UKVM service never enables pairing automatically.", addresses.len());
    println!("To receive a pairing request here, run `ukvm pair` in another terminal. To connect here, put the other computer in pairing mode.");
    if options.host.is_none() && !yes_no("Find/pair or configure another computer now?", false)? {
        println!("\nLocal setup complete. No new peer trust was granted.");
        setup_summary(&config, &trust_dir)?;
        return Ok(());
    }
    let store = TrustStore::load(&trust_dir)?;
    let selection = select_peer(&options, &store, &identity).await?;
    let (peer, live_verified) = match selection {
        Selection::Existing(candidate, peer) => {
            println!(
                "\n{} is in this computer's pinned trust store as '{}'.",
                candidate.display_name, peer.alias
            );
            println!("The mDNS identity claim has not authenticated the live device.");
            match verify_known(&candidate, &peer, &identity, &config.node.name, &trust_dir).await {
                Ok(address) => {
                    println!("Pinned mTLS identity verified at {address}.");
                    if options.debug {
                        eprintln!(
                            "setup verified peer ID={} via pinned TLS at {address}",
                            peer.id
                        );
                    }
                    let current = TrustStore::load(&trust_dir)?
                        .find_id(&peer.id)
                        .ok_or("trusted peer disappeared")?
                        .clone();
                    if address_changed(current.address.as_deref(), &address)
                        && yes_no(
                            &format!(
                                "Update {}'s connection address from {} to {address}?",
                                peer.alias,
                                current.address.as_deref().unwrap_or("none")
                            ),
                            false,
                        )?
                    {
                        let _lock = edit_lock(&trust_dir)?;
                        let mut store = TrustStore::load(&trust_dir)?;
                        store.set_address(&trust_dir, &peer.id, &address)?;
                    }
                    (
                        TrustStore::load(&trust_dir)?
                            .find_id(&peer.id)
                            .ok_or("trusted peer disappeared")?
                            .clone(),
                        true,
                    )
                }
                Err(error) => {
                    println!("Live identity check unavailable: {error}");
                    println!("The stored certificate remains pinned. No discovered address will be saved.");
                    if !yes_no(
                        "Continue configuring the existing local trust record offline?",
                        false,
                    )? {
                        return Err("setup cancelled; existing trust was preserved".into());
                    }
                    (peer, false)
                }
            }
        }
        Selection::New(candidate) => {
            show_recovery_hint(&trust_dir, &candidate)?;
            if !candidate.compatible(PAIRING_VERSION) {
                return Err(format!(
                    "{} advertises incompatible pairing protocol {}",
                    candidate.display_name, candidate.pairing_version
                ));
            }
            println!(
                "\nPairing with {}. Compare the verification code on both computers.",
                candidate.display_name
            );
            let stream = pairing::connect_discovered(&candidate).await?;
            let claim = candidate.advertised_node_id.clone();
            let peer = pair_new(stream, &identity, &config.node.name, &trust_dir, claim).await?;
            (peer, true)
        }
        Selection::Manual(host) => {
            println!("\nPairing with {host}. The address is only a connection hint.");
            let address = pairing::target_address(&host)?;
            let stream = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect(&address),
            )
            .await
            .map_err(|_| "pairing connection timed out".to_string())?
            .map_err(|e| e.to_string())?;
            let peer = pair_new(stream, &identity, &config.node.name, &trust_dir, None).await?;
            (peer, true)
        }
    };
    configure_permissions(&trust_dir, &peer)?;
    config = configure_layout(&config_path, &trust_dir, &peer)?;
    let peer = TrustStore::load(&trust_dir)?
        .find_id(&peer.id)
        .ok_or("peer disappeared during setup")?
        .clone();
    let capability = platform_capability().await;
    let edge = setup_model::current_edge(&config, &peer.id);
    println!("\nUniversalKVM setup complete.\n\nThis computer: {}\nPeer: {} ({})\nTrust: pinned{}\nLayout: {}\nLocal permissions for {}:\n  Connect: {}\n  Debug input to THIS computer: {}\n  Control THIS computer: {}\nPlatform:\n  Local capture: {}\n  Local injection: {}", config.node.name, peer.alias, peer.id, if live_verified { "; live identity verified" } else { "; live identity not verified this run" }, edge.map(|e| format!("{} -> {} -> {}", config.node.name, e.as_str(), peer.alias)).unwrap_or_else(|| "not configured".into()), peer.alias, peer.permissions.connect, peer.permissions.debug_input, peer.permissions.control_this_node, capability.capture, capability.injection);
    println!("\nRemote control requires control-this-node permission on the receiving computer and normal portal consent. Run `ukvm apply` to apply saved topology/name changes to the managed service.");
    setup_summary(&config, &trust_dir)?;
    Ok(())
}

enum Selection {
    Existing(DiscoveryResult, Peer),
    New(DiscoveryResult),
    Manual(String),
}

async fn select_peer(
    options: &Options,
    store: &TrustStore,
    identity: &Identity,
) -> Result<Selection, String> {
    if let Some(host) = &options.host {
        return Ok(Selection::Manual(host.clone()));
    }
    loop {
        println!(
            "\nSearching for nearby UKVM nodes for {} seconds...",
            DEFAULT_BROWSE.as_secs()
        );
        let candidates = match crate::discovery::browse(options.interface.as_deref()).await {
            Ok(found) => found,
            Err(error) => {
                println!("Discovery unavailable: {error}");
                Vec::new()
            }
        };
        println!("Nearby computers:");
        for (index, candidate) in candidates.iter().enumerate() {
            let status = match setup_model::classify(candidate, store, &identity.id) {
                CandidateStatus::SelfClaim => "claims this computer's ID; ignored",
                CandidateStatus::KnownHint(_) => "matches a pinned peer ID (unverified hint)",
                CandidateStatus::AliasConflict { .. } => "possible identity conflict",
                CandidateStatus::New => "not paired",
            };
            println!(
                "  {}. {} — {} — {}",
                index + 1,
                candidate.display_name,
                candidate
                    .addresses
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "no address".into()),
                status
            );
            if options.debug {
                eprintln!(
                    "setup discovered instance={} advertised_id={} addresses={:?}",
                    candidate.service_instance,
                    candidate.advertised_node_id.as_deref().unwrap_or("none"),
                    candidate.addresses
                );
            }
        }
        if candidates.is_empty() {
            println!("  none found");
        }
        println!("  m. Enter hostname/IP manually\n  r. Search again\n  0. Cancel setup");
        let answer = ask("Select a computer: ")?;
        match answer.as_str() {
            "m" => {
                let host = ask("Hostname/IP of a node already in pairing mode: ")?;
                pairing::target_address(&host)?;
                return Ok(Selection::Manual(host));
            }
            "r" => continue,
            "0" => return Err("setup cancelled".into()),
            _ => {}
        }
        let index: usize = match answer.parse() {
            Ok(value) => value,
            Err(_) => {
                println!("Select a listed number, m, r, or 0.");
                continue;
            }
        };
        let Some(candidate) = index
            .checked_sub(1)
            .and_then(|i| candidates.get(i))
            .cloned()
        else {
            println!("No such node.");
            continue;
        };
        match setup_model::classify(&candidate, store, &identity.id) {
            CandidateStatus::SelfClaim => {
                println!("This advertisement claims this computer's ID. No connection was made.");
            }
            CandidateStatus::AliasConflict {
                trusted,
                claimed_id,
            } => {
                println!("Identity conflict: '{}' is pinned to {} but this advertisement claims {}. No trust change was made.", trusted.alias, trusted.id, claimed_id.as_deref().unwrap_or("no node ID"));
                return Err("inspect both computers' full identities; use explicit peer management for any replacement".into());
            }
            CandidateStatus::KnownHint(peer) => return Ok(Selection::Existing(candidate, peer)),
            CandidateStatus::New => return Ok(Selection::New(candidate)),
        }
    }
}

async fn verify_known(
    candidate: &DiscoveryResult,
    peer: &Peer,
    identity: &Identity,
    local_name: &str,
    trust_dir: &Path,
) -> Result<String, String> {
    let certificate = cert_path(trust_dir, &peer.id);
    let port = candidate.control_port.unwrap_or(ukvm_network::DEFAULT_PORT);
    let mut last_error = "no resolved address".to_string();
    for address in candidate.addresses.iter().take(8) {
        let endpoint = SocketAddr::new(address.ip(), port);
        let endpoint = match address {
            SocketAddr::V6(v6) => SocketAddr::V6(std::net::SocketAddrV6::new(
                *v6.ip(),
                port,
                0,
                v6.scope_id(),
            )),
            _ => endpoint,
        };
        match tokio::time::timeout(
            Duration::from_secs(3),
            transport::connect(&endpoint.to_string(), identity, &certificate, local_name),
        )
        .await
        {
            Ok(Ok(mut connection)) if connection.peer.id == peer.id => {
                let _ = ukvm_network::write_frame(
                    &mut connection.stream,
                    &ukvm_protocol::WireMessage::SessionEnd,
                )
                .await;
                return Ok(endpoint.to_string());
            }
            Ok(Ok(_)) => last_error = "authenticated node ID did not match pinned peer".into(),
            Ok(Err(error)) => last_error = error,
            Err(_) => last_error = "connection timed out".into(),
        }
    }
    Err(last_error)
}

fn show_recovery_hint(trust_dir: &Path, candidate: &DiscoveryResult) -> Result<(), String> {
    let records = pairing_state::list(trust_dir)?;
    let candidates = setup_model::matching_recovery(&records, candidate);
    for record in candidates {
        if matches!(
            record.status(trust_dir),
            LocalStatus::Prepared | LocalStatus::CommittedUnconfirmed
        ) {
            println!(
                "A previous confirmed pairing may be recoverable (transaction {}).",
                record.transaction_id
            );
            println!("Run `ukvm pair status` here and `ukvm pair recover` on the other computer. Recovery requires matching authenticated records on both nodes.");
            if !yes_no("Start a new pairing ceremony instead?", false)? {
                return Err("setup stopped for pairing recovery inspection".into());
            }
            break;
        }
    }
    Ok(())
}

async fn pair_new(
    stream: tokio::net::TcpStream,
    identity: &Identity,
    local_name: &str,
    trust_dir: &Path,
    advertised_id: Option<String>,
) -> Result<Peer, String> {
    let task = pair_stream(
        stream,
        PairRole::Initiator,
        identity,
        local_name,
        trust_dir,
        move |offer| async move {
            if advertised_id
                .as_deref()
                .is_some_and(|hint| hint != offer.remote_id)
            {
                eprintln!("Discovered node ID did not match the cryptographically proven pairing identity. No trust was granted.");
                return Ok(false);
            }
            pairing::confirm(offer).await
        },
    );
    let outcome = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(PAIRING_TIMEOUT_SECS), task) => result.map_err(|_| "pairing timed out".to_string())?.map_err(|e| e.to_string())?,
        _ = tokio::signal::ctrl_c() => return Err("setup pairing cancelled locally; inspect `ukvm pair status` if both sides had already confirmed".into()),
    };
    println!("Pairing complete with {}. Local input permissions remain denied unless explicitly changed.", outcome.peer.alias);
    Ok(outcome.peer)
}

fn configure_permissions(trust_dir: &Path, peer: &Peer) -> Result<(), String> {
    println!(
        "\nLocal permissions for {} (this computer decides what {} may do here):",
        peer.alias, peer.alias
    );
    for (label, allowed) in [
        ("connect", peer.permissions.connect),
        ("debug-input", peer.permissions.debug_input),
        ("control-this-node", peer.permissions.control_this_node),
    ] {
        println!("  {label}: {}", if allowed { "allowed" } else { "denied" });
    }
    if !yes_no("Change permissions on THIS computer?", false)? {
        return Ok(());
    }
    for (permission, label) in [
        (
            SessionPermission::DebugInput,
            "send DebugSink input to THIS computer",
        ),
        (
            SessionPermission::ControlThisNode,
            "control THIS computer when an authorized injector exists",
        ),
    ] {
        println!("\n{} may {}:", peer.alias, label);
        println!("  1. Keep current setting\n  2. Allow\n  3. Deny");
        let choice = choice("Selection: ", &["1", "2", "3"])?;
        if choice == "1" {
            continue;
        }
        let allow = choice == "2";
        if !yes_no(
            &format!(
                "{} {} to {}?",
                if allow { "Allow" } else { "Deny" },
                peer.alias,
                label
            ),
            false,
        )? {
            continue;
        }
        let _lock = edit_lock(trust_dir)?;
        let mut store = TrustStore::load(trust_dir)?;
        store.set_permission(trust_dir, &peer.id, permission, allow)?;
    }
    Ok(())
}

fn configure_layout(config_path: &Path, trust_dir: &Path, peer: &Peer) -> Result<Config, String> {
    let mut config = ukvm_config::load_or_default(config_path).map_err(|e| e.to_string())?;
    if let Some(edge) = setup_model::current_edge(&config, &peer.id) {
        println!(
            "\n{} is currently {} of this computer.",
            peer.alias,
            edge.as_str()
        );
    }
    println!("Where is {} relative to this computer?\n  1. Left\n  2. Right\n  3. Above\n  4. Below\n  0. Keep current layout / skip", peer.alias);
    let selected = choice("Selection: ", &["0", "1", "2", "3", "4"])?;
    let edge = match selected.as_str() {
        "1" => Edge::Left,
        "2" => Edge::Right,
        "3" => Edge::Top,
        "4" => Edge::Bottom,
        _ => return Ok(config),
    };
    if peer.address.is_none() {
        return Err("peer has no verified/configured address; cannot place it in layout".into());
    }
    let occupied = config
        .layout
        .adjacent_link(edge)
        .filter(|link| link.peer_id != peer.id);
    let replace = if let Some(link) = &occupied {
        let store = TrustStore::load(trust_dir)?;
        let name = store
            .find_id(&link.peer_id)
            .map(|p| p.alias.as_str())
            .unwrap_or("another peer");
        yes_no(
            &format!(
                "{} edge currently belongs to {name}. Replace that link?",
                edge.as_str()
            ),
            false,
        )?
    } else {
        false
    };
    if occupied.is_some() && !replace {
        return Ok(config);
    }
    let mut preview = config.clone();
    setup_model::set_peer_edge(&mut preview, &peer.id, edge, replace)?;
    let remaining = preview.layout.topology();
    let removed: Vec<_> = config
        .layout
        .topology()
        .into_iter()
        .filter(|node| {
            node.peer_id != peer.id && !remaining.iter().any(|next| next.peer_id == node.peer_id)
        })
        .collect();
    if !removed.is_empty() {
        let store = TrustStore::load(trust_dir)?;
        let names = removed
            .iter()
            .map(|node| {
                store
                    .find_id(&node.peer_id)
                    .map(|peer| peer.alias.as_str())
                    .unwrap_or(node.peer_id.as_str())
            })
            .collect::<Vec<_>>()
            .join(", ");
        if !yes_no(&format!("This change removes topology placements for {names}; their trust is retained. Continue?"), false)? {
            return Ok(config);
        }
    }
    let approved_layout = config.layout.clone();
    let _lock = edit_lock(trust_dir)?;
    config = ukvm_config::load_or_default(config_path).map_err(|e| e.to_string())?;
    if config.layout != approved_layout {
        return Err(
            "layout changed during confirmation; retry without replacing other placements".into(),
        );
    }
    let store = TrustStore::load(trust_dir)?;
    if store
        .find_id(&peer.id)
        .and_then(|p| p.address.as_ref())
        .is_none()
    {
        return Err("peer trust or address changed during layout setup; retry".into());
    }
    if setup_model::set_peer_edge(&mut config, &peer.id, edge, replace)? {
        ukvm_config::save(config_path, &config).map_err(|e| e.to_string())?;
        println!(
            "Saved layout: {} -> {} -> {}",
            config.node.name,
            edge.as_str(),
            peer.alias
        );
    } else {
        println!("Layout already matches; no change made.");
    }
    Ok(config)
}

fn setup_summary(config: &Config, trust_dir: &Path) -> Result<(), String> {
    println!(
        "\nCurrent configuration: {}\nTrusted machines: {}\nConfigured positions: {}",
        config.node.name,
        TrustStore::load(trust_dir)?.peers.len(),
        config.layout.topology().len()
    );
    #[cfg(target_os = "linux")]
    println!("Service: {}", crate::service::summary());
    println!("Inspect: ukvm status | ukvm peers | ukvm layout show | ukvm doctor\nApply saved changes: ukvm apply");
    Ok(())
}

async fn platform_capability() -> setup_model::CapabilitySummary {
    #[cfg(target_os = "linux")]
    {
        let probe = match tokio::time::timeout(
            Duration::from_secs(5),
            ukvm_platform::linux_wayland::probe(),
        )
        .await
        {
            Ok(probe) => probe,
            Err(_) => {
                return setup_model::capability_summary(
                    false,
                    false,
                    "portal capability probe timed out",
                )
            }
        };
        let capture = probe.session.wayland_socket_reachable
            && probe.capture.version.is_some()
            && probe.capture.pointer_advertised
            && probe.capture.keyboard_advertised;
        let injection = probe.session.wayland_socket_reachable
            && probe.remote.version.is_some_and(|version| version >= 2)
            && probe.remote.pointer_advertised
            && probe.remote.keyboard_advertised;
        let mut summary = setup_model::capability_summary(
            capture,
            injection,
            "RemoteDesktop v2 keyboard/pointer or native Wayland session unavailable",
        );
        if let Some(error) = probe.capture.error {
            summary.capture.push_str(&format!("; {error}"));
        }
        if let Some(error) = probe.remote.error {
            summary.injection.push_str(&format!("; {error}"));
        }
        if let Some(error) = probe.portal_error {
            summary.capture.push_str(&format!("; {error}"));
        }
        summary
    }
    #[cfg(not(target_os = "linux"))]
    {
        setup_model::capability_summary(false, false, "platform backend not implemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_hint_with_default_port_is_unchanged() {
        assert!(!address_changed(Some("192.168.1.40"), "192.168.1.40:24800"));
        assert!(address_changed(Some("192.168.1.41"), "192.168.1.40:24800"));
    }

    #[test]
    fn setup_options_require_explicit_host_and_no_yes_bypass() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        assert_eq!(
            parse(&[
                "--interface".into(),
                "enp10s0".into(),
                "--host".into(),
                "192.168.1.40".into(),
                "--debug".into()
            ])
            .unwrap(),
            Options {
                interface: Some("enp10s0".into()),
                host: Some("192.168.1.40".into()),
                debug: true
            }
        );
        assert!(parse(&["--yes".into()]).is_err());
        assert!(parse(&["--host".into()]).is_err());
    }

    #[tokio::test]
    async fn known_peer_requires_pinned_tls_before_address_update() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-id")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-id")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let mut a_store = TrustStore::default();
        let b_peer = a_store
            .import(
                &a_dir,
                "io".into(),
                Some("192.168.1.40".into()),
                b.tls_cert().as_ref(),
            )
            .unwrap();
        let mut b_store = TrustStore::default();
        b_store
            .import(
                &b_dir,
                "spectrum".into(),
                Some("192.168.1.21".into()),
                a.tls_cert().as_ref(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let candidate = DiscoveryResult {
            service_instance: "io._ukvm._tcp.local.".into(),
            display_name: "io".into(),
            advertised_node_id: Some(b.id.clone()),
            pairing_version: 2,
            pairing_available: true,
            input_version: Some((1, 1)),
            control_port: Some(port),
            addresses: vec!["127.0.0.1:24801".parse().unwrap()],
        };
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            transport::accept_trusted(tcp, &b, &b_store, &b_dir, "io")
                .await
                .unwrap();
        };
        let client = verify_known(&candidate, &b_peer, &a, "spectrum", &a_dir);
        let ((), verified) = tokio::join!(server, client);
        assert_eq!(verified.unwrap(), format!("127.0.0.1:{port}"));
        assert_eq!(
            TrustStore::load(&a_dir)
                .unwrap()
                .find_id(&b.id)
                .unwrap()
                .address
                .as_deref(),
            Some("192.168.1.40")
        );
    }
}
