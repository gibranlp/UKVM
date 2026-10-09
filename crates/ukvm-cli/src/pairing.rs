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

pub(crate) fn target_address(target: &str) -> Result<String, String> {
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

pub(crate) async fn confirm(offer: PairOffer) -> Result<bool, String> {
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
                            pairing_available: true,
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
                println!("Local permissions: connect={} debug-input={} control-this-node={}. Configure layout and receiver permission separately.", outcome.peer.permissions.connect, outcome.peer.permissions.debug_input, outcome.peer.permissions.control_this_node);
                ExitCode::SUCCESS
            }
            Err(e) => super::fail(&e),
        }
    })
}

pub(crate) async fn connect_discovered(
    candidate: &DiscoveryResult,
) -> Result<tokio::net::TcpStream, String> {
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
            pairing_available: true,
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

fn json_event(event: &str, data: serde_json::Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::json!({"schema_version": 1, "command": "pair", "event": event, "data": data})
    );
    io::stdout().flush().map_err(|e| e.to_string())
}

fn decode_confirmation(line: &str, offer: &PairOffer) -> Result<bool, String> {
    let reply: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| format!("invalid pairing confirmation JSON: {e}"))?;
    if reply["remote_node_id"] != offer.remote_id
        || reply["verification_code"] != offer.verification_code
    {
        return Err("confirmation does not match the current peer and verification code".into());
    }
    reply["accept"]
        .as_bool()
        .ok_or_else(|| "confirmation requires a boolean accept field".into())
}

async fn confirm_json(offer: PairOffer) -> Result<bool, String> {
    json_event(
        "confirmation_required",
        serde_json::json!({"remote_name": offer.remote_name, "remote_node_id": offer.remote_id, "address_hint": offer.address, "verification_code": offer.verification_code, "already_trusted_as": offer.already_trusted_as}),
    )?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let mut line = String::new();
        let result = io::stdin()
            .lock()
            .take(4097)
            .read_line(&mut line)
            .map_err(|e| e.to_string())
            .and_then(|count| {
                if count == 0 {
                    return Ok(false);
                }
                if count > 4096 || !line.ends_with('\n') {
                    return Err("confirmation must be one JSON line of at most 4096 bytes".into());
                }
                decode_confirmation(&line, &offer)
            });
        let _ = sender.send(result);
    });
    receiver
        .await
        .map_err(|_| "pairing confirmation cancelled".to_string())?
}

pub(crate) fn pair_json(args: &[String]) -> ExitCode {
    let result = (|| -> Result<_, String> {
        if args.iter().filter(|a| a.as_str() == "--json").count() != 1 {
            return Err("specify --json exactly once".into());
        }
        let mut plain = Vec::new();
        let mut expected_id = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--json" => {}
                "--expected-node-id" if expected_id.is_none() => {
                    let id = iter
                        .next()
                        .ok_or("--expected-node-id requires a full Node ID")?;
                    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("invalid expected Node ID".into());
                    }
                    expected_id = Some(id.to_ascii_lowercase());
                }
                _ => plain.push(arg.clone()),
            }
        }
        let target = parse_target(&plain).map_err(str::to_string)?;
        if matches!(target, Target::Discover(_)) {
            return Err("use discover --json, select a pairing endpoint, then pair HOST --json --expected-node-id ID".into());
        }
        let config =
            ukvm_config::load_or_default(&ukvm_config::default_path().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let control_port = config
            .network
            .listen
            .parse::<std::net::SocketAddr>()
            .map_err(|e| e.to_string())?
            .port();
        let identity = Identity::load_or_create(&default_state_dir()?)?;
        let trust_dir = default_trust_dir()?;
        super::with_runtime(async move {
            let task = async {
                let mut registration = None;
                let mut discovery = None;
                let (stream, role) = match target {
                    Target::Direct(host) => {
                        let address = target_address(host)?;
                        json_event(
                            "connecting",
                            serde_json::json!({"endpoint": address, "timeout_seconds": PAIRING_TIMEOUT_SECS}),
                        )?;
                        (
                            tokio::net::TcpStream::connect(&address)
                                .await
                                .map_err(|e| format!("pairing connection: {e}"))?,
                            PairRole::Initiator,
                        )
                    }
                    Target::Listen {
                        bind,
                        interface,
                        advertise,
                        ..
                    } => {
                        let listener = tokio::net::TcpListener::bind(&bind)
                            .await
                            .map_err(|e| format!("pairing listener: {e}"))?;
                        let endpoint = listener.local_addr().map_err(|e| e.to_string())?;
                        if advertise {
                            let advertisement = Advertisement {
                                node_name: config.node.name.clone(),
                                node_id: identity.id.clone(),
                                pairing_port: endpoint.port(),
                                control_port,
                                pairing_available: true,
                                pairing_version: PAIRING_VERSION,
                                input_version: (
                                    ukvm_protocol::PROTOCOL_MAJOR,
                                    ukvm_protocol::PROTOCOL_MINOR,
                                ),
                            };
                            match DiscoveryService::new(interface) {
                                Ok(service) => match service.advertise(&advertisement).await {
                                    Ok(guard) => {
                                        registration = Some(guard);
                                        discovery = Some(service);
                                    }
                                    Err(error) => json_event(
                                        "discovery_unavailable",
                                        serde_json::json!({"message": error, "direct_pairing_available": true}),
                                    )?,
                                },
                                Err(error) => json_event(
                                    "discovery_unavailable",
                                    serde_json::json!({"message": error, "direct_pairing_available": true}),
                                )?,
                            }
                        }
                        json_event(
                            "listening",
                            serde_json::json!({"endpoint": endpoint.to_string(), "node_id": identity.id, "name": config.node.name, "discovery_available": registration.is_some(), "timeout_seconds": PAIRING_TIMEOUT_SECS}),
                        )?;
                        (
                            listener.accept().await.map_err(|e| e.to_string())?.0,
                            PairRole::Responder,
                        )
                    }
                    Target::Discover(_) => unreachable!(),
                };
                let result = pair_stream(
                    stream,
                    role,
                    &identity,
                    &config.node.name,
                    &trust_dir,
                    move |offer| async move {
                        if expected_id
                            .as_deref()
                            .is_some_and(|id| id != offer.remote_id)
                        {
                            return Err(
                                "cryptographically proven peer differs from expected Node ID"
                                    .into(),
                            );
                        }
                        confirm_json(offer).await
                    },
                )
                .await
                .map_err(|e| e.to_string());
                drop(registration);
                drop(discovery);
                result
            };
            #[cfg(unix)]
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .map_err(|e| e.to_string())?;
            #[cfg(unix)]
            let terminated = terminate.recv();
            #[cfg(not(unix))]
            let terminated = std::future::pending::<()>();
            tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(PAIRING_TIMEOUT_SECS), task) => result.map_err(|_| "pairing expired; inspect pair status before retrying".to_string())?,
                _ = tokio::signal::ctrl_c() => Err("pairing cancelled locally; inspect pair status before retrying".into()),
                _ = terminated => Err("pairing terminated locally; inspect pair status before retrying".into()),
            }
        })
    })();
    let (ok, data, error) = match result {
        Ok(outcome) => (
            true,
            serde_json::json!({"peer": outcome.peer, "created": outcome.created, "apply_command": "ukvm apply"}),
            serde_json::Value::Null,
        ),
        Err(message) => (
            false,
            serde_json::Value::Null,
            serde_json::json!({"code": "pairing_failed", "message": message}),
        ),
    };
    println!(
        "{}",
        serde_json::json!({"schema_version": 1, "command": "pair", "event": "result", "ok": ok, "data": data, "error": error})
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;
    #[test]
    fn confirmation_is_bounded_to_the_current_verified_peer_and_code() {
        let offer = PairOffer {
            remote_name: "io".into(),
            remote_id: "a".repeat(64),
            verification_code: "example-code".into(),
            address: "127.0.0.1".into(),
            already_trusted_as: None,
        };
        let reply = |accept: serde_json::Value, id: &str, code: &str| {
            serde_json::json!({"accept": accept, "remote_node_id": id, "verification_code": code})
                .to_string()
        };
        assert!(decode_confirmation(
            &reply(
                serde_json::json!(true),
                &offer.remote_id,
                &offer.verification_code
            ),
            &offer
        )
        .unwrap());
        assert!(!decode_confirmation(
            &reply(
                serde_json::json!(false),
                &offer.remote_id,
                &offer.verification_code
            ),
            &offer
        )
        .unwrap());
        assert!(decode_confirmation(
            &reply(
                serde_json::json!(true),
                &"b".repeat(64),
                &offer.verification_code
            ),
            &offer
        )
        .is_err());
        assert!(decode_confirmation(
            &reply(serde_json::json!(true), &offer.remote_id, "other-code"),
            &offer
        )
        .is_err());
        assert!(decode_confirmation(
            &reply(
                serde_json::json!("yes"),
                &offer.remote_id,
                &offer.verification_code
            ),
            &offer
        )
        .is_err());
        assert!(decode_confirmation("{broken", &offer).is_err());
    }
}
