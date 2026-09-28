use std::{
    io::{self, Write},
    process::ExitCode,
    time::Duration,
};
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

pub fn pair(args: &[String]) -> ExitCode {
    if args.first().is_some_and(|v| v == "status") {
        return pair_status(&args[1..]);
    }
    if args.first().is_some_and(|v| v == "recover") {
        return pair_recover(&args[1..]);
    }
    if args.first().is_some_and(|v| v == "cleanup") {
        return pair_cleanup(&args[1..]);
    }
    let (target, bind) = match args {
        [] => (None, format!("0.0.0.0:{PAIRING_PORT}")),
        [target] if !target.starts_with('-') => (Some(target.as_str()), String::new()),
        [flag, address] if flag == "--bind" => (None, address.clone()),
        _ => return super::fail("usage: ukvm pair [HOST[:PORT]] or ukvm pair --bind ADDR"),
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
    let local_name = config.node.name;
    super::with_runtime(async move {
        println!("UniversalKVM Pairing\nNode: {local_name}\nPairing protocol: {PAIRING_VERSION}\nExpires after {PAIRING_TIMEOUT_SECS} seconds.");
        let task = async {
            let (stream, role) = if let Some(target) = target {
                let address = target_address(target)?;
                println!("Connecting to: {address}");
                let stream = tokio::net::TcpStream::connect(&address)
                    .await
                    .map_err(|e| format!("pairing connection: {e}"))?;
                (stream, PairRole::Initiator)
            } else {
                let listener = tokio::net::TcpListener::bind(&bind)
                    .await
                    .map_err(|e| format!("pairing listener: {e}"))?;
                println!(
                    "Waiting for one pairing attempt on {}...",
                    listener.local_addr().map_err(|e| e.to_string())?
                );
                let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
                (stream, PairRole::Responder)
            };
            pair_stream(stream, role, &identity, &local_name, &trust_dir, confirm)
                .await
                .map_err(|e| e.to_string())
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
