use serde_json::{json, Value};
use std::process::ExitCode;
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let command = args.first()?;
    if command != "send" && command != "files" {
        return None;
    }
    let json = args.iter().any(|a| a == "--json");
    if args.iter().filter(|a| a.as_str() == "--json").count() > 1 {
        return Some(output(
            command,
            Err("--json may only be specified once".into()),
            json,
        ));
    }
    if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
        println!(
            "{}",
            if command == "send" {
                "Usage: ukvm send PATH... PEER [--json]\nThe receiving computer must authorize receive-files. Symlinks/special files are rejected."
            } else {
                "Usage: ukvm files [status|directory [ABSOLUTE_PATH]|cancel TRANSFER_ID] [--json]"
            }
        );
        return Some(ExitCode::SUCCESS);
    }
    let plain = args[1..]
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    let result = if command == "send" {
        return Some(send(&plain, json));
    } else {
        manage(&plain)
    };
    #[cfg(not(target_os = "linux"))]
    let result: Result<Value, String> =
        Err("file transfer filesystem support currently requires Linux".into());
    Some(output(command, result, json))
}
fn output(command: &str, result: Result<Value, String>, structured: bool) -> ExitCode {
    let ok = result.is_ok();
    let value = match result {
        Ok(data) => {
            json!({"schema_version":1,"command":command,"ok":true,"data":data,"error":null})
        }
        Err(error) => {
            json!({"schema_version":1,"command":command,"ok":false,"data":null,"error":{"code":"operation_failed","message":error}})
        }
    };
    if structured {
        println!("{value}");
    } else if ok {
        println!(
            "{}",
            serde_json::to_string_pretty(&value["data"]).unwrap_or_default()
        );
    } else {
        eprintln!(
            "{}",
            value["error"]["message"]
                .as_str()
                .unwrap_or("file operation failed")
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn status_data() -> Result<Value, String> {
    let path = ukvm_config::default_path().map_err(|e| e.to_string())?;
    let config = ukvm_config::load_or_default(&path).map_err(|e| e.to_string())?;
    let destination = config
        .file_transfer
        .receive_directory
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(ukvm_platform::files::linux::default_receive_directory)?;
    let history =
        ukvm_platform::files::history::History::new(&ukvm_network::trust::default_trust_dir()?)?
            .list()?;
    let restart = matches!(ukvm_runtime::ownership::status(),Ok(ukvm_runtime::ownership::RuntimeStatus::Running(ref s))if s.file_protocol_version!=Some(1));
    Ok(
        json!({"supported":true,"protocol_version":1,"receive_directory":destination,"max_chunk_bytes":ukvm_protocol::files::CHUNK_BYTES,"max_entries":ukvm_protocol::files::MAX_ENTRIES,"bytes_per_second":ukvm_protocol::files::BYTES_PER_SECOND,"state":if restart{"restart_required"}else{"available"},"transfers":history}),
    )
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn status_data() -> Result<Value, String> {
    Ok(json!({"supported":false,"state":"unsupported","transfers":[]}))
}
#[cfg(target_os = "linux")]
fn manage(args: &[String]) -> Result<Value, String> {
    use ukvm_network::trust::{default_trust_dir, edit_lock};
    match args {
        [] => status_data(),
        [action] if action == "status" || action == "directory" => status_data(),
        [action, id] if action == "cancel" => {
            ukvm_platform::files::history::History::new(&default_trust_dir()?)?.cancel(id)?;
            Ok(json!({"id":id,"cancellation_requested":true}))
        }
        [action, path] if action == "directory" => {
            let selected = std::path::PathBuf::from(path);
            if !selected.is_absolute() {
                return Err("receive directory must be an absolute path".into());
            }
            let _lock = edit_lock(&default_trust_dir()?)?;
            let location = ukvm_config::default_path().map_err(|e| e.to_string())?;
            let mut config = ukvm_config::load_or_default(&location).map_err(|e| e.to_string())?;
            config.file_transfer.receive_directory = Some(path.clone());
            ukvm_config::save(&location, &config).map_err(|e| e.to_string())?;
            Ok(json!({"receive_directory":path,"apply_required":true,"apply_command":"ukvm apply"}))
        }
        _ => Err(
            "usage: ukvm files [status|directory [ABSOLUTE_PATH]|cancel TRANSFER_ID] [--json]"
                .into(),
        ),
    }
}
#[cfg(target_os = "linux")]
fn send(args: &[String], structured: bool) -> ExitCode {
    let result = prepare_send(args);
    let (source, peer, identity, dir, history, record) = match result {
        Ok(value) => value,
        Err(error) => return send_result(Err(error), structured),
    };
    let id = record.status.id.clone();
    let row = std::sync::Arc::new(std::sync::Mutex::new(record));
    let observed = std::sync::Arc::clone(&row);
    if structured {
        println!(
            "{}",
            json!({"schema_version":1,"command":"send","event":"started","data":row.lock().unwrap().status})
        );
    } else {
        eprintln!(
            "Sending {} → {}",
            row.lock().unwrap().status.name,
            peer.alias
        );
    }
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&stopped);
    let trust = dir.clone();
    let peer_id = peer.id.clone();
    let state = history.clone();
    let cancel_id = id.clone();
    let check: ukvm_network::files::Check = std::sync::Arc::new(move || {
        if flag.load(std::sync::atomic::Ordering::Acquire) || state.cancelled(&cancel_id) {
            return Err("transfer cancelled".into());
        }
        ukvm_network::trust::TrustStore::load(&trust)?
            .authorize(&peer_id, ukvm_network::trust::SessionPermission::Connect)?;
        Ok(())
    });
    let observer: ukvm_network::files::Observer = std::sync::Arc::new(move |progress| {
        let mut row = observed
            .lock()
            .map_err(|_| "transfer history lock failed")?;
        row.status.bytes = progress.bytes;
        row.status.state = progress.state.into();
        row.status.destination = progress.destination;
        row.update(true)?;
        if structured {
            println!(
                "{}",
                json!({"schema_version":1,"command":"send","event":"progress","data":row.status})
            );
        } else {
            let percent = if row.status.total_bytes == 0 {
                100.0
            } else {
                100.0 * row.status.bytes as f64 / row.status.total_bytes as f64
            };
            let seconds = row.status.updated.saturating_sub(row.status.started).max(1);
            eprintln!(
                "{} / {} bytes  {:.0}%  {:.1} MiB/s",
                row.status.bytes,
                row.status.total_bytes,
                percent,
                row.status.bytes as f64 / seconds as f64 / 1048576.0
            );
        }
        Ok(())
    });
    let result = super::with_runtime(async {
        let shutdown = async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .map_err(|e| e.to_string())?;
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            stopped.store(true, std::sync::atomic::Ordering::Release);
            Ok::<(), String>(())
        };
        let transfer = async {
            let mut addresses = vec![];
            if let Some(address) = &peer.address {
                addresses.push(ukvm_network::trust::resolve_address(address)?);
            }
            let found = ukvm_network::files::guarded(super::discovery::browse_all(None), &check)
                .await?
                .unwrap_or_default();
            for machine in found
                .iter()
                .filter(|m| m.advertised_node_id.as_deref() == Some(&peer.id))
            {
                if let Some(port) = machine.control_port {
                    for address in &machine.addresses {
                        let mut address = *address;
                        address.set_port(port);
                        let address = address.to_string();
                        if !addresses.contains(&address) {
                            addresses.push(address);
                        }
                    }
                }
            }
            let mut connected = None;
            let mut problem = "Peer unavailable; start UKVM on the receiving computer".to_string();
            for address in addresses.into_iter().take(8) {
                match ukvm_network::files::guarded(
                    ukvm_network::files::connect(
                        &address,
                        &identity,
                        &ukvm_network::trust::cert_path(&dir, &peer.id),
                        &peer.id,
                    ),
                    &check,
                )
                .await?
                {
                    Ok(stream) => {
                        connected = Some(stream);
                        break;
                    }
                    Err(error) => problem = error,
                }
            }
            let mut stream = connected.ok_or(problem)?;
            ukvm_network::files::send(&mut stream, Box::new(source), check, observer).await
        };
        tokio::pin!(transfer);
        tokio::select! {result=&mut transfer=>result,signal=shutdown=>{signal?;transfer.await}}
    });
    {
        let mut row = row.lock().unwrap();
        if let Err(error) = &result {
            row.status.state = if error.contains("cancel") {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            row.status.error = Some(error.clone());
        }
        if let Err(error) = row.update(true) {
            return send_result(Err(error), structured);
        }
    }
    send_result(result.map(|destination|json!({"id":id,"destination":destination,"bytes":row.lock().unwrap().status.bytes})),structured)
}
#[cfg(target_os = "linux")]
type Prepared = (
    ukvm_platform::files::linux::Source,
    ukvm_network::trust::Peer,
    ukvm_network::identity::Identity,
    std::path::PathBuf,
    ukvm_platform::files::history::History,
    ukvm_platform::files::history::Record,
);
#[cfg(target_os = "linux")]
fn prepare_send(args: &[String]) -> Result<Prepared, String> {
    use ukvm_network::{
        identity::{default_state_dir, Identity},
        trust::{default_trust_dir, SessionPermission, TrustStore},
    };
    if args.len() < 2 {
        return Err("usage: ukvm send PATH... PEER [--json]".into());
    }
    let dir = default_trust_dir()?;
    let store = TrustStore::load(&dir)?;
    let peer = store
        .find(args.last().ok_or("missing peer")?)
        .ok_or("unknown trusted peer")?
        .clone();
    store.authorize(&peer.id, SessionPermission::Connect)?;
    let identity = Identity::load_or_create(&default_state_dir()?)?;
    let id = ukvm_network::files::new_id()?;
    let source = ukvm_platform::files::linux::Source::new(
        &args[..args.len() - 1]
            .iter()
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>(),
        id.clone(),
    )?;
    use ukvm_core::files::FileSource;
    let offer = source.offer();
    let history = ukvm_platform::files::history::History::new(&dir)?;
    let record = history.begin(ukvm_core::files::TransferStatus {
        id,
        peer_id: peer.id.clone(),
        peer_name: peer.alias.clone(),
        direction: "send".into(),
        name: if args.len() == 2 {
            offer.entries[0].path[0].clone()
        } else {
            format!("{} selected paths", args.len() - 1)
        },
        state: "connecting".into(),
        bytes: 0,
        total_bytes: offer.total_bytes,
        entries: offer.entries.len(),
        started: 0,
        updated: 0,
        destination: None,
        error: None,
    })?;
    Ok((source, peer, identity, dir, history, record))
}
#[cfg(target_os = "linux")]
fn send_result(result: Result<Value, String>, structured: bool) -> ExitCode {
    if !structured {
        return output("send", result, false);
    }
    let ok = result.is_ok();
    let record = match result {
        Ok(data) => {
            json!({"schema_version":1,"command":"send","event":"result","ok":true,"data":data,"error":null})
        }
        Err(message) => {
            json!({"schema_version":1,"command":"send","event":"result","ok":false,"data":null,"error":{"code":"operation_failed","message":message}})
        }
    };
    println!("{record}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
