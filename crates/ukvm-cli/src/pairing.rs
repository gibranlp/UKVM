use std::{
    io::{self, Write},
    process::ExitCode,
    time::Duration,
};
use ukvm_network::{
    identity::{default_state_dir, Identity},
    pairing::{pair_stream, PairOffer, PairRole, PAIRING_PORT, PAIRING_TIMEOUT_SECS},
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
        println!("UniversalKVM Pairing\nNode: {local_name}\nPairing protocol: 1\nExpires after {PAIRING_TIMEOUT_SECS} seconds.");
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
            pair_stream(stream, role, &identity, &local_name, &trust_dir, confirm).await
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
