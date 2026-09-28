use std::{
    io::{self, Write},
    process::ExitCode,
};
use ukvm_discovery::{DiscoveryResult, DiscoveryService, DEFAULT_BROWSE};
use ukvm_network::pairing::PAIRING_VERSION;

const DISCOVER_HELP: &str = "Usage: ukvm discover [--interface NAME]\nBrowse for nearby pairing nodes for four seconds. Results are untrusted address hints.";

fn parse_discover_args(args: &[String]) -> Result<Option<&str>, &'static str> {
    match args {
        [] => Ok(None),
        [flag, name] if flag == "--interface" && !name.starts_with('-') => Ok(Some(name)),
        _ => Err(DISCOVER_HELP),
    }
}

pub async fn browse(interface: Option<&str>) -> Result<Vec<DiscoveryResult>, String> {
    let service = DiscoveryService::new(interface)?;
    service.browse_for(DEFAULT_BROWSE).await
}

pub fn print_candidates(candidates: &[DiscoveryResult]) {
    println!("Found {} UniversalKVM node(s).", candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        println!("\n[{}] {}", index + 1, candidate.display_name);
        for address in candidate.pairing_addresses() {
            println!("    Address: {address}");
        }
        println!(
            "    Pairing protocol: {}{}",
            candidate.pairing_version,
            if candidate.compatible(PAIRING_VERSION) {
                ""
            } else {
                " (incompatible)"
            }
        );
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
        match browse(interface).await {
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
