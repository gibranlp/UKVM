use std::{
    io::{self, Write},
    process::ExitCode,
    time::Duration,
};
use ukvm_discovery::{Advertisement, DiscoveryResult, DiscoveryService};
use ukvm_network::{
    identity::{default_state_dir, Identity},
    pairing::{
        pair_stream, recover_stream, PairOffer, PairRole, RecoveryOutcome, PAIRING_PORT,
        PAIRING_TIMEOUT_SECS, PAIRING_VERSION,
    },
    pairing_state::{self, LocalStatus},
    trust::default_trust_dir,
};

fn target_address(target: &str) -> Result<String, String> {
    if target.is_empty()
        || target
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '/')
    {
        return Err("invalid pairing address".into());
    }
    if target.starts_with('[') {
        if target.contains("]:") {
            Ok(target.into())
        } else if target.ends_with(']') {
            Ok(format!("{target}:{PAIRING_PORT}"))
        } else {
            Err("invalid IPv6 pairing address".into())
        }
    } else if target.contains(':') {
        if target.matches(':').count() != 1 {
            return Err("IPv6 pairing addresses must use brackets".into());
        }
        Ok(target.into())
    } else {
        Ok(format!("{target}:{PAIRING_PORT}"))
    }
}

async fn confirm(offer: PairOffer) -> Result<bool, String> {
    println!("\nSecure pairing channel established.\nRemote node: {}\nNode ID: {}\nAddress hint: {}\nVerification code: {}",offer.remote_name,offer.remote_id,offer.address,offer.verification_code);
    if let Some(alias) = &offer.already_trusted_as {
        println!("This identity is already trusted as '{alias}'. Existing permissions will be preserved.");
    }
    println!("Compare this code with the other computer. Both must show the same value.");
    print!("Trust this node? [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut answer = String::new();
        let value = io::stdin()
            .read_line(&mut answer)
            .map(|_| {
                answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes")
            })
            .map_err(|e| e.to_string());
        let _ = sender.send(value);
    });
    receiver
        .await
        .map_err(|_| "pairing confirmation input cancelled".to_string())?
}

const PAIR_HELP: &str = "Usage: ukvm pair [HOST[:PORT]|--discover [--interface NAME]|--interface NAME [--debug]|--debug|--bind ADDR]\n\
Listening advertises through mDNS for 90 seconds; --debug prints advertiser details.\n\
Direct HOST pairing and pair recover do not use discovery.";

#[derive(Debug, PartialEq)]
enum Target<'a> {
    Listen {
        bind: String,
        interface: Option<&'a str>,
        advertise: bool,
        debug: bool,
    },
    Direct(&'a str),
    Discover(Option<&'a str>),
}

fn parse_target(args: &[String]) -> Result<Target<'_>, &'static str> {
    let default_bind = || format!("0.0.0.0:{PAIRING_PORT}");
    match args {
        [] => Ok(Target::Listen {
            bind: default_bind(),
            interface: None,
            advertise: true,
            debug: false,
        }),
        [flag] if flag == "--debug" => Ok(Target::Listen {
            bind: default_bind(),
            interface: None,
            advertise: true,
            debug: true,
        }),
        [flag] if flag == "--discover" => Ok(Target::Discover(None)),
        [flag, option, name]
            if flag == "--discover" && option == "--interface" && !name.starts_with('-') =>
        {
            Ok(Target::Discover(Some(name)))
        }
        [flag, name] if flag == "--interface" && !name.starts_with('-') => Ok(Target::Listen {
            bind: default_bind(),
            interface: Some(name),
            advertise: true,
            debug: false,
        }),
        [flag, name, debug]
            if flag == "--interface" && !name.starts_with('-') && debug == "--debug" =>
        {
            Ok(Target::Listen {
                bind: default_bind(),
                interface: Some(name),
                advertise: true,
                debug: true,
            })
        }
        [debug, flag, name]
            if debug == "--debug" && flag == "--interface" && !name.starts_with('-') =>
        {
            Ok(Target::Listen {
                bind: default_bind(),
                interface: Some(name),
                advertise: true,
                debug: true,
            })
        }
        [flag, address] if flag == "--bind" => Ok(Target::Listen {
            bind: address.clone(),
            interface: None,
            advertise: false,
            debug: false,
        }),
        [host] if !host.starts_with('-') => Ok(Target::Direct(host)),
        _ => Err(PAIR_HELP),
    }
}

pub fn pair(args: &[String]) -> ExitCode {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("{PAIR_HELP}");
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|v| v == "status") {
        return pair_status(&args[1..]);
    }
    if args.first().is_some_and(|v| v == "recover") {
        return pair_recover(&args[1..]);
    }
    if args.first().is_some_and(|v| v == "cleanup") {
        return pair_cleanup(&args[1..]);
    }
    let target = match parse_target(args) {
        Ok(target) => target,
        Err(help) => return super::fail(help),
    };
    let config = match ukvm_config::default_path()
        .map_err(|e| e.to_string())
        .and_then(|p| ukvm_config::load_or_default(&p).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => return super::fail(&e),
    };
    let identity = match default_state_dir().and_then(|p| Identity::load_or_create(&p)) {
        Ok(i) => i,
        Err(e) => return super::fail(&e),
    };
    let trust_dir = match default_trust_dir() {
        Ok(p) => p,
        Err(e) => return super::fail(&e),
    };
    let control_port = match config.network.listen.parse::<std::net::SocketAddr>() {
        Ok(addr) => addr.port(),
        Err(e) => return super::fail(&format!("configured network listen address: {e}")),
    };
    let local_name = config.node.name;
    super::with_runtime(async move {
        println!("UniversalKVM Pairing\nNode: {local_name}\nPairing protocol: {PAIRING_VERSION}\nExpires after {PAIRING_TIMEOUT_SECS} seconds.");
        let task = async {
            let mut discovery_service = None::<DiscoveryService>;
            let mut advertised = None;
            let (stream, role) = match target {
                Target::Direct(host) => {
                    let address = target_address(host)?;
                    println!("Connecting to: {address}");
                    (
                        tokio::net::TcpStream::connect(&address)
                            .await
                            .map_err(|e| format!("pairing connection: {e}"))?,
                        PairRole::Initiator,
                    )
                }
                Target::Discover(interface) => {
                    let candidate = super::discovery::select_candidate(interface).await?;
                    let stream = connect_discovered(&candidate).await?;
                    (stream, PairRole::Initiator)
                }
                Target::Listen {
                    bind,
                    interface,
                    advertise,
                    debug,
                } => {
                    let listener = tokio::net::TcpListener::bind(&bind)
                        .await
                        .map_err(|e| format!("pairing listener: {e}"))?;
                    let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
                    if advertise {
                        let announcement = Advertisement {
                            node_name: local_name.clone(),
                            node_id: identity.id.clone(),
                            pairing_port: local_addr.port(),
                            control_port,
                            pairing_version: PAIRING_VERSION,
                            input_version: (
                                ukvm_protocol::PROTOCOL_MAJOR,
                                ukvm_protocol::PROTOCOL_MINOR,
                            ),
                        };
                        if debug {
                            eprintln!("mDNS advertiser starting: service type={} node={} pairing port={} control port={} input protocol={}.{} interface={}", ukvm_discovery::SERVICE_TYPE, local_name, local_addr.port(), control_port, ukvm_protocol::PROTOCOL_MAJOR, ukvm_protocol::PROTOCOL_MINOR, interface.unwrap_or("eligible non-loopback interfaces"));
                            match ukvm_discovery::local_interface_addresses(interface) {
                                Ok(addresses) if addresses.is_empty() => eprintln!("mDNS local interface addresses: none matched; registration may fail"),
                                Ok(addresses) => for address in addresses { eprintln!("mDNS local interface address: {address}"); },
                                Err(error) => eprintln!("mDNS interface diagnostics unavailable: {error}"),
                            }
                        }
                        match DiscoveryService::new(interface) {
                            Ok(service) => match service.advertise(&announcement).await {
                                Ok(guard) => {
                                    if debug {
                                        eprintln!("mDNS registration announced: instance={} hostname={} target={}", guard.fullname(), guard.hostname(), guard.announcement_target());
                                    }
                                    advertised = Some(guard);
                                    discovery_service = Some(service);
                                    println!("Discoverable by mDNS while pairing is active.");
                                },
                                Err(e) => eprintln!("Discovery advertisement unavailable: {e}; direct pairing still works."),
                            },
                            Err(e) => eprintln!("Discovery advertisement unavailable: {e}; direct pairing still works."),
                        }
                    }
                    println!("Waiting for one pairing attempt on {local_addr}...");
                    let stream = loop {
                        tokio::select! {
                            accepted = listener.accept() => break accepted.map_err(|e| e.to_string())?.0,
                            _ = tokio::time::sleep(Duration::from_millis(250)), if discovery_service.is_some() => {
                                if let Some(error) = discovery_service.as_ref().and_then(DiscoveryService::take_error) {
                                    eprintln!("Discovery advertisement stopped: {error}; direct pairing remains available.");
                                    advertised.take();
                                    discovery_service.take();
                                }
                            }
                        }
                    };
                    (stream, PairRole::Responder)
                }
            };
            let result = pair_stream(stream, role, &identity, &local_name, &trust_dir, confirm)
                .await
                .map_err(|e| e.to_string());
            drop(advertised);
            drop(discovery_service);
            result
        };
        let result = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(PAIRING_TIMEOUT_SECS),task)=>result.map_err(|_|"pairing expired".to_string()).and_then(|r|r),
            _=tokio::signal::ctrl_c()=>Err("pairing cancelled locally".into()),
        };
        match result {
            Ok(outcome) => {
                println!(
                    "Pairing successful with {} ({}).",
                    outcome.peer.alias, outcome.peer.id
                );
                if !outcome.created {
                    println!("Existing trust record was preserved.");
                }
                println!("Remote input permissions remain denied. Set layout and permissions separately.");
                ExitCode::SUCCESS
            }
            Err(e) => super::fail(&e),
        }
    })
}

async fn connect_discovered(candidate: &DiscoveryResult) -> Result<tokio::net::TcpStream, String> {
    let mut last_error = String::from("no usable address");
    for address in candidate.pairing_addresses().iter().take(8) {
        println!("Connecting to discovered address: {address}");
        match tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(address),
        )
        .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => last_error = "connection timed out".into(),
        }
    }
    Err(format!(
        "could not reach discovered node '{}': {last_error}; discovery is only an address hint",
        candidate.display_name
    ))
}

fn pair_status(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return super::fail("usage: ukvm pair status");
    }
    let dir = match default_trust_dir() {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    let records = match pairing_state::list(&dir) {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    println!("UniversalKVM Pairing Status");
    if records.is_empty() {
        println!("No pairing recovery records.");
    }
    for record in records {
        let status = match record.status(&dir) {
            LocalStatus::Prepared => "prepared; no local trust (recovery or new ceremony needed)",
            LocalStatus::CommittedUnconfirmed => "local trust committed; remote outcome uncertain",
            LocalStatus::Complete => "completed locally; receipt retained for peer recovery",
            LocalStatus::Conflict => "CONFLICT; inspect trust store manually",
            LocalStatus::Expired => "expired; cannot recover (cleanup available)",
        };
        println!(
            "\nRemote: {}\nNode ID: {}\nTransaction: {}\nState: {status}",
            record.remote_name, record.remote_id, record.transaction_id
        );
    }
    ExitCode::SUCCESS
}

fn pair_cleanup(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        return super::fail("usage: ukvm pair cleanup");
    }
    let dir = match default_trust_dir() {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    match pairing_state::cleanup_expired(&dir) {
        Ok(n) => {
            println!(
                "Removed {n} expired pairing recovery records. Trust records were not changed."
            );
            ExitCode::SUCCESS
        }
        Err(e) => super::fail(&e),
    }
}

fn pair_recover(args: &[String]) -> ExitCode {
    let (target, supplied_tx) = match args {
        [] => (None, None),
        [target] if !target.starts_with('-') => (Some(target.as_str()), None),
        [target, tx] if !target.starts_with('-') => (Some(target.as_str()), Some(tx.as_str())),
        _ => return super::fail("usage: ukvm pair recover [HOST[:PORT] [TRANSACTION_ID]]"),
    };
    let config = match ukvm_config::default_path()
        .map_err(|e| e.to_string())
        .and_then(|p| ukvm_config::load_or_default(&p).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    let identity = match default_state_dir().and_then(|p| Identity::load_or_create(&p)) {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    let dir = match default_trust_dir() {
        Ok(v) => v,
        Err(e) => return super::fail(&e),
    };
    let tx = if target.is_some() {
        match supplied_tx {
            Some(v) => Some(v.to_string()),
            None => {
                let records = match pairing_state::list(&dir) {
                    Ok(v) => v,
                    Err(e) => return super::fail(&e),
                };
                let active: Vec<_> = records
                    .into_iter()
                    .filter(|r| r.status(&dir) != LocalStatus::Expired)
                    .collect();
                let uncertain: Vec<_> = active
                    .iter()
                    .filter(|r| r.status(&dir) != LocalStatus::Complete)
                    .collect();
                let selected = if uncertain.len() == 1 {
                    Some(uncertain[0])
                } else if uncertain.is_empty() && active.len() == 1 {
                    Some(&active[0])
                } else {
                    None
                };
                let Some(selected) = selected else {
                    return super::fail("specify a transaction ID from 'ukvm pair status' when there is not exactly one unexpired record");
                };
                Some(selected.transaction_id.clone())
            }
        }
    } else {
        None
    };
    super::with_runtime(async move {
        println!("UniversalKVM Pairing Recovery\nNode: {}\nRecovery uses the original confirmed transaction and pinned certificate.", config.node.name);
        let task = async {
            let (stream, role) = if let Some(target) = target {
                let addr = target_address(target)?;
                println!("Connecting to: {addr}");
                (
                    tokio::net::TcpStream::connect(&addr)
                        .await
                        .map_err(|e| e.to_string())?,
                    PairRole::Initiator,
                )
            } else {
                let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{PAIRING_PORT}"))
                    .await
                    .map_err(|e| e.to_string())?;
                println!(
                    "Waiting for one recovery attempt on {}...",
                    listener.local_addr().map_err(|e| e.to_string())?
                );
                (
                    listener.accept().await.map_err(|e| e.to_string())?.0,
                    PairRole::Responder,
                )
            };
            recover_stream(
                stream,
                role,
                &identity,
                &config.node.name,
                &dir,
                tx.as_deref(),
            )
            .await
        };
        let result = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(PAIRING_TIMEOUT_SECS),task)=>result.map_err(|_|"recovery timed out; inspect 'ukvm pair status'".to_string()).and_then(|r|r),
            _=tokio::signal::ctrl_c()=>Err("recovery cancelled; inspect 'ukvm pair status'".into()),
        };
        match result {
            Ok(RecoveryOutcome::Completed(peer)) => {
                println!("Pairing complete with {} ({}). Existing permissions and layout were preserved.",peer.alias,peer.id);
                ExitCode::SUCCESS
            }
            Ok(RecoveryOutcome::RestartRequired) => {
                println!("Neither node committed trust. Start a new pairing ceremony.");
                ExitCode::SUCCESS
            }
            Err(e) => super::fail(&e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukvm_network::{identity::Identity, trust::TrustStore};

    #[test]
    fn pairing_interface_and_debug_options_parse() {
        let interface = vec!["--interface".into(), "enp0s20u2u1u2".into()];
        assert!(matches!(
            parse_target(&interface),
            Ok(Target::Listen {
                interface: Some("enp0s20u2u1u2"),
                advertise: true,
                debug: false,
                ..
            })
        ));
        let debug = vec![
            "--interface".into(),
            "enp0s20u2u1u2".into(),
            "--debug".into(),
        ];
        assert!(matches!(
            parse_target(&debug),
            Ok(Target::Listen {
                interface: Some("enp0s20u2u1u2"),
                advertise: true,
                debug: true,
                ..
            })
        ));
        assert!(matches!(
            parse_target(&["--discover".into()]),
            Ok(Target::Discover(None))
        ));
        assert!(PAIR_HELP.starts_with("Usage: ukvm pair"));
    }

    #[test]
    fn direct_address_still_uses_pairing_port() {
        assert_eq!(
            target_address("192.168.1.40").unwrap(),
            "192.168.1.40:24801"
        );
        assert_eq!(target_address("[::1]").unwrap(), "[::1]:24801");
        assert_eq!(
            target_address("192.168.1.40:1234").unwrap(),
            "192.168.1.40:1234"
        );
    }

    #[tokio::test]
    async fn discovered_address_hands_off_to_existing_protocol_two() {
        let temp = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(&temp.path().join("a-id")).unwrap();
        let b = Identity::load_or_create(&temp.path().join("b-id")).unwrap();
        let a_dir = temp.path().join("a-trust");
        let b_dir = temp.path().join("b-trust");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let candidate = DiscoveryResult {
            service_instance: "untrusted.local.".into(),
            display_name: "claimed-io".into(),
            advertised_node_id: Some("0".repeat(64)),
            pairing_version: PAIRING_VERSION,
            input_version: Some((1, 1)),
            control_port: Some(24800),
            addresses: vec!["127.0.0.1:1".parse().unwrap(), address],
        };
        let responder = async {
            let (stream, _) = listener.accept().await.unwrap();
            pair_stream(stream, PairRole::Responder, &b, "io", &b_dir, |_| async {
                Ok(true)
            })
            .await
            .unwrap()
        };
        let initiator = async {
            let stream = connect_discovered(&candidate).await.unwrap();
            pair_stream(
                stream,
                PairRole::Initiator,
                &a,
                "spectrum",
                &a_dir,
                |offer| async move {
                    // The advertisement is a hint: the authenticated identity and
                    // name come from the existing Protocol 2 exchange.
                    assert_eq!(offer.remote_name, "io");
                    Ok(true)
                },
            )
            .await
            .unwrap()
        };
        let (a_result, b_result) = tokio::join!(initiator, responder);
        assert!(a_result.created && b_result.created);
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
        assert!(!a_peer.permissions.debug_input && !a_peer.permissions.control_this_node);
        assert!(!b_peer.permissions.debug_input && !b_peer.permissions.control_this_node);
        assert!(TrustStore::load(&a_dir)
            .unwrap()
            .find_id(&"0".repeat(64))
            .is_none());
    }
}
