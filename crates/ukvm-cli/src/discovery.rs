use std::{
    io::{self, Write},
    process::ExitCode,
};
use ukvm_discovery::{DiscoveryResult, DiscoveryService, DEFAULT_BROWSE};
use ukvm_network::pairing::PAIRING_VERSION;

const DISCOVER_HELP: &str = "Usage: ukvm discover [--interface NAME]\nBrowse nearby pairing and running nodes for four seconds. Results are untrusted address hints.";

fn parse_discover_args(args: &[String]) -> Result<Option<&str>, &'static str> {
    match args {
        [] => Ok(None),
        [flag, name] if flag == "--interface" && !name.starts_with('-') => Ok(Some(name)),
        _ => Err(DISCOVER_HELP),
    }
}

pub async fn browse(interface: Option<&str>) -> Result<Vec<DiscoveryResult>, String> {
    let service = DiscoveryService::new(interface)?;
    service
        .browse_for(DEFAULT_BROWSE)
        .await
        .map(|hints| hints.into_iter().filter(|h| h.pairing_available).collect())
}

pub async fn browse_all(interface: Option<&str>) -> Result<Vec<DiscoveryResult>, String> {
    DiscoveryService::new(interface)?
        .browse_for(DEFAULT_BROWSE)
        .await
}

pub fn print_candidates(candidates: &[DiscoveryResult]) {
    let store = ukvm_network::trust::default_trust_dir()
        .and_then(|dir| ukvm_network::trust::TrustStore::load(&dir));
    let config = ukvm_config::default_path()
        .ok()
        .and_then(|path| ukvm_config::load_or_default(&path).ok());

    println!("Found {} UniversalKVM node(s).", candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        println!("\n[{}] {}", index + 1, candidate.display_name);
        for address in candidate.pairing_addresses() {
            println!("    Address: {address}");
        }
        println!(
            "    Pairing protocol: {}{}",
            candidate.pairing_version,
            if !candidate.pairing_available {
                " (not in pairing mode)"
            } else if candidate.compatible(PAIRING_VERSION) {
                ""
            } else {
                " (incompatible)"
            }
        );
        println!("    Advertised Node ID: {} (unverified)\n    Pairing available: {}\n    Control port: {}", candidate.advertised_node_id.as_deref().unwrap_or("unknown"), candidate.pairing_available, candidate.control_port.map(|p| p.to_string()).unwrap_or_else(|| "unknown".into()));
        let pinned = store.as_ref().ok().and_then(|store| {
            candidate
                .advertised_node_id
                .as_deref()
                .and_then(|id| store.find_id(id))
        });
        println!(
            "    Availability: discovered; live identity unverified\n    Trust: {}",
            if pinned.is_some() {
                "advertised ID matches a local pin; authentication still required"
            } else {
                "no verified trust from this advertisement"
            }
        );
        if let Some(peer) = pinned {
            println!(
                "    Local control-this-node: {}",
                peer.permissions.control_this_node
            );
            if let Some(position) = config.as_ref().and_then(|config| {
                config
                    .layout
                    .topology()
                    .into_iter()
                    .find(|node| node.peer_id == peer.id)
            }) {
                println!("    Configured position: {}", position.coordinate());
            }
        }
        if let Err(error) = &store {
            println!("    Local trust state unavailable: {error}");
        }
        if let Some((major, minor)) = candidate.input_version {
            println!("    Input protocol hint: {major}.{minor}");
        }
    }
}

pub fn discover(args: &[String]) -> ExitCode {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("{DISCOVER_HELP}");
        return ExitCode::SUCCESS;
    }
    let interface = match parse_discover_args(args) {
        Ok(interface) => interface,
        Err(help) => return super::fail(help),
    };
    super::with_runtime(async move {
        println!(
            "Searching for UniversalKVM nodes for {} seconds...",
            DEFAULT_BROWSE.as_secs()
        );
        match browse_all(interface).await {
            Ok(candidates) => {
                print_candidates(&candidates);
                if candidates.is_empty() {
                    println!("Check that another node is in pairing mode and that multicast UDP 5353 is allowed on the selected LAN interface.");
                }
                println!("Discovery entries are untrusted hints. Pairing still requires matching codes and approval on both nodes.");
                ExitCode::SUCCESS
            }
            Err(error) => super::fail(&format!("discovery failed: {error}. Check multicast UDP 5353, interfaces, and local firewall; direct 'ukvm pair HOST' remains available")),
        }
    })
}

pub async fn select_candidate(interface: Option<&str>) -> Result<DiscoveryResult, String> {
    println!(
        "Searching for pairing nodes for {} seconds...",
        DEFAULT_BROWSE.as_secs()
    );
    let candidates = browse(interface).await?;
    print_candidates(&candidates);
    if candidates.is_empty() {
        return Err("no UKVM pairing node found; confirm the other node is running 'ukvm pair', or use direct 'ukvm pair HOST'".into());
    }
    print!("\nSelect node [1-{}]: ", candidates.len());
    io::stdout().flush().map_err(|e| e.to_string())?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut answer = String::new();
        let selection = io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                answer
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "enter a listed number".to_string())
            });
        let _ = sender.send(selection);
    });
    let selected = receiver
        .await
        .map_err(|_| "selection cancelled".to_string())??;
    let candidate = candidates
        .get(selected.checked_sub(1).ok_or("invalid selection")?)
        .ok_or("selection is outside the displayed list")?;
    if !candidate.compatible(PAIRING_VERSION) {
        return Err(format!("{} advertises pairing protocol {}; this build requires {}. The actual handshake also verifies the version", candidate.display_name, candidate.pairing_version, PAIRING_VERSION));
    }
    Ok(candidate.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_and_help_parse_as_discovery() {
        assert_eq!(parse_discover_args(&[]), Ok(None));
        assert_eq!(
            parse_discover_args(&["--interface".into(), "enp10s0".into()]),
            Ok(Some("enp10s0"))
        );
        assert!(parse_discover_args(&["--help".into()]).is_err());
        assert!(DISCOVER_HELP.starts_with("Usage: ukvm discover"));
    }
}
