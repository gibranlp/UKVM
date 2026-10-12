use serde_json::{json, Value};
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};
use ukvm_config::Config;
use ukvm_network::{
    identity::{certificate_id, default_state_dir},
    trust::{default_trust_dir, edit_lock, SessionPermission, TrustStore},
};

const SCHEMA: u32 = 1;

pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let command = args.first()?;
    let json_output = args.iter().any(|arg| arg == "--json");
    if !json_output
        && !matches!(
            command.as_str(),
            "layout" | "unpair" | "apply" | "authorization"
        )
        && !(command == "peer"
            && (args.get(1).is_some_and(|a| a == "remove")
                || args
                    .get(3)
                    .is_some_and(|a| a == "receive-files" || a == "view-screen")))
    {
        return None;
    }
    if command == "pair" && json_output {
        let plain: Vec<_> = args
            .iter()
            .skip(1)
            .filter(|a| a.as_str() != "--json")
            .collect();
        if !(plain.len() == 1 && plain[0] == "status") {
            return Some(super::pairing::pair_json(&args[1..]));
        }
    }
    let rest: Vec<_> = args
        .iter()
        .skip(1)
        .filter(|arg| arg.as_str() != "--json")
        .cloned()
        .collect();
    let needs_confirmation = (command == "authorization"
        && rest.first().is_some_and(|arg| arg == "reset")
        || command == "unpair"
        || command == "peer" && rest.first().is_some_and(|arg| arg == "remove")
        || command == "layout"
            && rest
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "remove" | "remove-position")))
        && !rest.iter().any(|arg| arg == "--yes");
    if !json_output && rest == ["--help"] {
        println!("{}", match command.as_str() {
            "layout" => "Usage: ukvm layout [show|place X Y PEER|move PEER X Y|remove PEER [--yes]|remove-position X Y [--yes]|set EDGE PEER] [--json]\nCoordinates are cardinal distance 1-2 from local (0,0); outer nodes require an inner neighbor. Save changes, then run ukvm apply.",
            "unpair" => "Usage: ukvm unpair PEER [--yes] [--json]\nRemoves local trust and its topology branch. Remote trust is unchanged.",
            "authorization" => "Usage: ukvm authorization [status|reset [--yes]] [--json]\nReset stops the managed runtime and forgets both remembered portal grants. Peer trust/permissions stay unchanged; Start requests normal consent again.",
            "apply" => "Usage: ukvm apply [--json]\nValidate configuration and restart the managed user service; normal portal consent may be required.",
            _ => "Usage: ukvm peer remove PEER [--yes]",
        });
        return Some(ExitCode::SUCCESS);
    }
    let result = if json_output && needs_confirmation {
        Err("confirmation required; frontend must ask the user, then explicitly pass --yes".into())
    } else if args.iter().filter(|arg| arg.as_str() == "--json").count() > 1 {
        Err("--json may only be specified once".into())
    } else {
        execute(command, &rest)
    };
    Some(match result {
        Ok(data) => {
            if json_output {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA, "command": command, "ok": true, "data": data, "error": null})
                );
            } else {
                print_human(command, &data);
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            if json_output {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA, "command": command, "ok": false, "data": null, "error": {"code": "operation_failed", "message": message}})
                );
                ExitCode::FAILURE
            } else {
                super::fail(&message)
            }
        }
    })
}

fn context() -> Result<(Config, TrustStore), String> {
    let config =
        ukvm_config::load_or_default(&ukvm_config::default_path().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let store = TrustStore::load(&default_trust_dir()?)?;
    if identity()?.is_some_and(|id| store.find_id(&id).is_some()) {
        return Err("this node is listed as its own peer; inspect local trust before applying configuration".into());
    }
    Ok((config, store))
}

fn identity() -> Result<Option<String>, String> {
    match std::fs::read(default_state_dir()?.join("node.der")) {
        Ok(bytes) => Ok(Some(certificate_id(&bytes))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn runtime() -> Value {
    #[cfg(target_os = "linux")]
    {
        use ukvm_runtime::ownership::{status, RuntimeStatus};
        match status() {
            Ok(RuntimeStatus::Running(snapshot)) => {
                json!({"state": "running", "snapshot": *snapshot, "error": null, "freshness_seconds": 5})
            }
            Ok(RuntimeStatus::Stopped) => {
                json!({"state": "stopped", "snapshot": null, "error": null, "freshness_seconds": 5})
            }
            Ok(RuntimeStatus::StartingOrUnavailable(error)) => {
                json!({"state": "owned_unknown", "snapshot": null, "error": error, "freshness_seconds": 5})
            }
            Err(error) => {
                json!({"state": "unknown", "snapshot": null, "error": error, "freshness_seconds": 5})
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    json!({"state": "unsupported", "snapshot": null, "error": "Linux required", "freshness_seconds": 5})
}

fn service() -> Value {
    #[cfg(target_os = "linux")]
    {
        match super::service::properties() {
            Ok(state) => {
                json!({"available": true, "load_state": state.get("LoadState"), "active_state": state.get("ActiveState"), "sub_state": state.get("SubState"), "unit_file_state": state.get("UnitFileState"), "main_pid": state.get("MainPID").and_then(|s| s.parse::<u32>().ok()), "error": null})
            }
            Err(error) => {
                json!({"available": false, "load_state": null, "active_state": null, "sub_state": null, "unit_file_state": null, "main_pid": null, "error": error})
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    json!({"available": false, "error": "Linux required"})
}

fn availability(runtime: &Value, id: &str) -> Value {
    let observed = runtime["snapshot"]["peers"]
        .as_array()
        .and_then(|peers| peers.iter().find(|p| p["id"] == id))
        .and_then(|p| p["availability"].as_str());
    let state = match observed {
        Some("Ready" | "Active" | "Authenticated") => "online",
        Some("Unavailable") => "unavailable",
        Some("Stopped") => "offline",
        _ => "unknown",
    };
    json!({"state": state, "runtime_state": observed, "source": if observed.is_some() {"runtime_snapshot"} else {"none"}})
}

fn clipboard_status(store: &TrustStore, runtime: &Value) -> Value {
    let enabled = store
        .peers
        .iter()
        .any(|p| p.permissions.connect && p.permissions.share_clipboard);
    let mut value = runtime["snapshot"]["clipboard"].clone();
    if !value.is_object() {
        value = json!({"supported": null, "connected_peers": 0, "last_sync": null, "state": if enabled { "stopped" } else { "disabled" }, "error": null});
    }
    if runtime["state"] == "running" && value["state"] == "" {
        value["state"] = json!("restart_required");
        value["error"] = json!("Restart UKVM to load clipboard support");
    }
    value["enabled"] = json!(enabled);
    value["protocol_version"] = json!(ukvm_protocol::clipboard::CLIPBOARD_VERSION);
    value["max_text_bytes"] = json!(ukvm_protocol::clipboard::MAX_CLIPBOARD_BYTES);
    value
}

fn peers(config: &Config, store: &TrustStore, runtime: &Value) -> Vec<Value> {
    let topology = config.layout.topology();
    store.peers.iter().map(|peer| {
        let position = topology.iter().find(|n| n.peer_id == peer.id).map(|n| json!({"x": n.x, "y": n.y}));
        json!({"name": peer.alias, "node_id": peer.id, "trusted": true, "permissions": peer.permissions, "endpoint": peer.address, "position": position, "availability": availability(runtime, &peer.id)})
    }).collect()
}

fn layout(config: &Config, store: &TrustStore) -> Value {
    let mut nodes = vec![
        json!({"local": true, "name": config.node.name, "node_id": identity().ok().flatten(), "x": 0, "y": 0, "trusted": true, "endpoint": null}),
    ];
    let mut issues = Vec::new();
    for node in config.layout.topology() {
        let peer = store.find_id(&node.peer_id);
        if peer.is_none() {
            issues.push(json!({"code": "untrusted_peer", "node_id": node.peer_id}));
        }
        nodes.push(json!({"local": false, "name": peer.map(|p| &p.alias), "node_id": node.peer_id, "x": node.x, "y": node.y, "trusted": peer.is_some(), "endpoint": peer.and_then(|p| p.address.as_ref())}));
    }
    json!({"nodes": nodes, "issues": issues, "valid": issues.is_empty(), "coordinate_model": "cardinal_distance_1_2", "apply_command": "ukvm apply"})
}

fn execute(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "doctor" if args.is_empty() => doctor(),
        "authorization" => authorization_command(args),
        "setup" => initialize_local(args),
        "pair" if args == ["status"] => {
            let dir = default_trust_dir()?;
            let records: Vec<_> = ukvm_network::pairing_state::list(&dir)?.into_iter().map(|record| {
                use ukvm_network::pairing_state::LocalStatus;
                let state = match record.status(&dir) { LocalStatus::Prepared => "prepared", LocalStatus::CommittedUnconfirmed => "committed_unconfirmed", LocalStatus::Complete => "complete", LocalStatus::Conflict => "conflict", LocalStatus::Expired => "expired" };
                json!({"remote_node_id": record.remote_id, "remote_name": record.remote_name, "transaction_id": record.transaction_id, "state": state})
            }).collect();
            Ok(json!({"records": records}))
        }
        "discover" => discover(args),
        "status" | "peers" if args.is_empty() => {
            let (config, store) = context()?;
            let runtime = runtime();
            let peers = peers(&config, &store, &runtime);
            let service = service();
            if command == "peers" {
                return Ok(json!({"peers": peers}));
            }
            Ok(
                json!({"node": {"name": config.node.name, "node_id": identity()?}, "runtime": runtime, "service": service, "peers": peers, "layout": layout(&config, &store), "health": {"configuration_valid": layout(&config, &store)["valid"], "identity_initialized": identity()?.is_some(), "runtime_reporting": runtime["state"] == "running", "service_active": service["active_state"] == "active"}, "clipboard": clipboard_status(&store, &runtime), "files": super::files::status_data()?, "desktop_authorization": super::authorization::status(None, None), "configuration": {"apply_command": "ukvm apply", "loaded_at": "runtime_start", "disk_matches_runtime": null}, "limitations": ["hyprland_partial_external_edge_barriers"]}),
            )
        }
        "layout" => edit_layout(args),
        "unpair" => unpair(args),
        "peer" if args.first().is_some_and(|a| a == "remove") => unpair(&args[1..]),
        "peer" => permission(args),
        "service" => service_command(args),
        "apply" if args.is_empty() => apply_configuration(),
        _ => Err("invalid command/arguments for the management interface; run ukvm help".into()),
    }
}

fn initialize_local(args: &[String]) -> Result<Value, String> {
    let name = match args {
        [local] if local == "--local" => None,
        [local, flag, name] if local == "--local" && flag == "--name" => Some(name),
        _ => return Err("usage: ukvm setup --local [--name NAME] --json; local identity/configuration only, never pairing, consent or service activation".into()),
    };
    let _lock = edit_lock(&default_trust_dir()?)?;
    let path = ukvm_config::default_path().map_err(|e| e.to_string())?;
    let mut config = ukvm_config::load_or_default(&path).map_err(|e| e.to_string())?;
    let mut changed = !path.exists();
    if let Some(name) = name {
        changed |= config.node.name != *name;
        config.node.name = name.clone();
    }
    config.validate().map_err(|e| e.to_string())?;
    let initialized = identity()?.is_none();
    let identity = ukvm_network::identity::Identity::load_or_create(&default_state_dir()?)?;
    if changed {
        ukvm_config::save(&path, &config).map_err(|e| e.to_string())?;
    }
    Ok(
        json!({"node": {"name": config.node.name, "node_id": identity.id},
        "initialized": initialized, "configuration_changed": changed,
        "apply_required": initialized || changed, "apply_command": "ukvm apply",
        "consent_requested": false, "service_started": false, "trust_changed": false}),
    )
}

pub fn doctor() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let report = super::with_runtime(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ukvm_platform::linux_wayland::probe(),
            )
            .await
        })
        .map_err(|_| "portal capability probe timed out")?;
        let interface = |p: &ukvm_platform::linux_wayland::InterfaceProbe| json!({"version": p.version, "keyboard_advertised": p.keyboard_advertised, "pointer_advertised": p.pointer_advertised, "error": p.error});
        let backend_state = if !report.portal_service
            || report.capture.version.is_none()
            || report.remote.version.is_none()
        {
            "backend_missing"
        } else if report.capture.version.unwrap_or(0) < 1
            || report.remote.version.unwrap_or(0) < 2
            || !report.capture.keyboard_advertised
            || !report.capture.pointer_advertised
            || !report.remote.keyboard_advertised
            || !report.remote.pointer_advertised
        {
            "incompatible_backend"
        } else {
            "backend_ready"
        };
        Ok(
            json!({"backend_state": backend_state, "os": "linux", "session": {"type_hint": report.session.session_type_hint, "desktop_hint": report.session.desktop_hint, "wayland_display": report.session.wayland_display, "wayland_socket_reachable": report.session.wayland_socket_reachable}, "portal": {"available": report.portal_service, "error": report.portal_error}, "input_capture": interface(&report.capture), "remote_desktop": interface(&report.remote), "connect_to_eis_available": report.remote.version.is_some_and(|v| v >= 2), "consent_requested": false, "authorization": "not_tested", "desktop_authorization": super::authorization::status(report.remote.version, report.capture.version), "clipboard": {"supported": super::with_runtime(ukvm_platform::clipboard::wayland::supported()), "backend": "wayland_data_control", "protocol_version": ukvm_protocol::clipboard::CLIPBOARD_VERSION, "max_text_bytes": ukvm_protocol::clipboard::MAX_CLIPBOARD_BYTES, "contents_read": false}, "physical_input": "not_tested", "limitations": ["hyprland_partial_external_edge_barriers"]}),
        )
    }
    #[cfg(not(target_os = "linux"))]
    Err("doctor management interface currently requires Linux".into())
}

fn discover(args: &[String]) -> Result<Value, String> {
    let interface = match args {
        [] => None,
        [flag, name] if flag == "--interface" && !name.starts_with('-') => Some(name.as_str()),
        _ => return Err("usage: ukvm discover [--interface NAME] [--json]".into()),
    };
    let candidates = super::with_runtime(super::discovery::browse_all(interface))?;
    let (config, store) = context()?;
    let machines: Vec<_> = candidates.iter().map(|candidate| {
        let peer = candidate.advertised_node_id.as_deref().and_then(|id| store.find_id(id));
        let position = peer.and_then(|p| config.layout.topology().into_iter().find(|n| n.peer_id == p.id)).map(|n| json!({"x": n.x, "y": n.y}));
        json!({"name": candidate.display_name, "advertised_node_id": candidate.advertised_node_id, "service_instance": candidate.service_instance, "addresses": candidate.addresses.iter().map(ToString::to_string).collect::<Vec<_>>(), "control_port": candidate.control_port, "pairing_available": candidate.pairing_available, "pairing_version": candidate.pairing_version, "input_version": candidate.input_version, "untrusted_hint": true, "matches_trusted_id": peer.is_some(), "local_permissions": peer.map(|p| p.permissions), "configured_position": position, "availability": "discovered_unverified"})
    }).collect();
    Ok(json!({"machines": machines, "browse_seconds": ukvm_discovery::DEFAULT_BROWSE.as_secs()}))
}

fn confirm(prompt: &str, yes: bool) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("confirmation required; run interactively or explicitly pass --yes".into());
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("cancelled; no changes applied".into())
    }
}

fn edit_layout(args: &[String]) -> Result<Value, String> {
    if args.is_empty() || args == ["show"] {
        let (config, store) = context()?;
        return Ok(layout(&config, &store));
    }
    let yes = args.last().is_some_and(|a| a == "--yes");
    let args = if yes { &args[..args.len() - 1] } else { args };
    let (config, store) = context()?;
    let mut next = config.clone();
    match args {
        [action, x, y, name] if action == "place" => {
            let (x, y) = coordinates(x, y)?;
            let peer = store.find(name).ok_or("unknown trusted peer")?;
            if identity()?.as_deref() == Some(&peer.id) { return Err("cannot place this node as a remote peer".into()); }
            if next.layout.topology().iter().any(|n| n.x == x && n.y == y) { return Err("coordinate is occupied; move or remove its peer first".into()); }
            next.set_position(x, y, peer.id.clone()).map_err(|e| e.to_string())?;
        }
        [action, name, x, y] if action == "move" => {
            let (x, y) = coordinates(x, y)?;
            let peer = store.find(name).ok_or("unknown trusted peer")?;
            let nodes = next.layout.topology();
            if !nodes.iter().any(|n| n.peer_id == peer.id) { return Err("peer has no placement; use layout place".into()); }
            if nodes.iter().any(|n| n.peer_id != peer.id && n.x == x && n.y == y) { return Err("coordinate is occupied".into()); }
            next.layout.nodes = nodes.into_iter().filter(|n| n.peer_id != peer.id).collect();
            next.layout.links.clear();
            next.set_position(x, y, peer.id.clone()).map_err(|e| e.to_string())?;
        }
        [action, name] if action == "remove" && store.find(name).is_some() => {
            let peer = store.find(name).ok_or("unknown trusted peer")?;
            let node = next.layout.topology().into_iter().find(|n| n.peer_id == peer.id).ok_or("peer has no placement")?;
            next.remove_position(node.x, node.y).map_err(|e| e.to_string())?;
            confirm(&format!("Remove {} from layout?", peer.alias), yes)?;
        }
        [action, x, y] if action == "remove-position" => {
            let (x, y) = coordinates(x, y)?;
            next.remove_position(x, y).map_err(|e| e.to_string())?;
            confirm(&format!("Remove layout position ({x},{y})?"), yes)?;
        }
        [action, edge, name] if action == "set" => {
            let edge = ukvm_config::Edge::parse(edge)?;
            let peer = store.find(name).ok_or("unknown trusted peer")?;
            let coordinate = edge.coordinate();
            if next.layout.topology().iter().any(|n| n.coordinate() == coordinate && n.peer_id != peer.id) { return Err("coordinate is occupied; remove its peer first".into()); }
            next.set_link(edge, peer.id.clone());
        }
        [action, edge] if action == "remove" => {
            let edge = ukvm_config::Edge::parse(edge)?;
            let coordinate = edge.coordinate();
            next.remove_position(coordinate.x, coordinate.y).map_err(|e| e.to_string())?;
            confirm(&format!("Remove {} layout link?", edge.as_str()), yes)?;
        }
        _ => return Err("usage: ukvm layout [show|place X Y PEER|move PEER X Y|remove PEER [--yes]|remove-position X Y [--yes]|set EDGE PEER] [--json]".into()),
    }
    next.validate().map_err(|e| e.to_string())?;
    for node in next.layout.topology() {
        if store.find_id(&node.peer_id).is_none() {
            return Err("layout references an untrusted peer".into());
        }
    }
    let _lock = edit_lock(&default_trust_dir()?)?;
    let (current, current_store) = context()?;
    if current != config || current_store.peers != store.peers {
        return Err("configuration/trust changed during edit; retry".into());
    }
    ukvm_config::save(
        &ukvm_config::default_path().map_err(|e| e.to_string())?,
        &next,
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"changed": next != config, "layout": layout(&next, &store), "apply_required": next != config, "apply_command": "ukvm apply"}),
    )
}

fn coordinates(x: &str, y: &str) -> Result<(i32, i32), String> {
    Ok((
        x.parse().map_err(|_| "x must be a signed integer")?,
        y.parse().map_err(|_| "y must be a signed integer")?,
    ))
}

fn unpair(args: &[String]) -> Result<Value, String> {
    let (name, yes) = match args {
        [name] => (name, false),
        [name, flag] if flag == "--yes" => (name, true),
        _ => return Err("usage: ukvm unpair PEER [--yes] [--json]".into()),
    };
    let (config, store) = context()?;
    let peer = store.find(name).ok_or("unknown trusted peer")?.clone();
    confirm(
        &format!(
            "Unpair {} [{}] and remove its topology branch?",
            peer.alias, peer.id
        ),
        yes,
    )?;
    let _lock = edit_lock(&default_trust_dir()?)?;
    let (mut current, mut current_store) = context()?;
    if current != config || current_store.peers != store.peers {
        return Err("configuration/trust changed during confirmation; retry".into());
    }
    current.remove_peer_from_layout(&peer.id);
    ukvm_config::save(
        &ukvm_config::default_path().map_err(|e| e.to_string())?,
        &current,
    )
    .map_err(|e| e.to_string())?;
    current_store.remove(&default_trust_dir()?, &peer.id)?;
    Ok(
        json!({"removed_node_id": peer.id, "name": peer.alias, "apply_required": true, "apply_command": "ukvm apply", "remote_trust_removed": false}),
    )
}

fn permission(args: &[String]) -> Result<Value, String> {
    if let [action, name] = args {
        if action == "show" || action == "permissions" {
            let store = TrustStore::load(&default_trust_dir()?)?;
            let peer = store.find(name).ok_or("unknown trusted peer")?;
            return Ok(
                json!({"node_id": peer.id, "name": peer.alias, "permissions": peer.permissions, "scope": "this_computer"}),
            );
        }
    }

    let (action, name, permission) =
        match args {
            [action, name, permission] if action == "allow" || action == "deny" => {
                (action, name, permission)
            }
            _ => return Err(
                "usage: ukvm peer allow|deny PEER connect|debug-input|control-this-node|share-clipboard|receive-files|view-screen [--json]"
                    .into(),
            ),
        };
    let permission = match permission.as_str() {
        "connect" => SessionPermission::Connect,
        "debug-input" => SessionPermission::DebugInput,
        "control-this-node" => SessionPermission::ControlThisNode,
        "share-clipboard" => SessionPermission::ShareClipboard,
        "view-screen" => SessionPermission::ViewScreen,
        "receive-files" => SessionPermission::ReceiveFiles,
        _ => return Err("unknown permission".into()),
    };
    if permission == SessionPermission::ShareClipboard {
        let live = runtime();
        if live["state"] == "running" && live["snapshot"]["clipboard"]["state"] == "" {
            return Err("Restart UKVM to load M19 before changing clipboard permissions".into());
        }
    }
    if permission == SessionPermission::ViewScreen {
        let live = runtime();
        if live["state"] == "running" && live["snapshot"]["display_protocol_version"] != json!(1) {
            return Err(
                "Restart UKVM to load remote sessions before changing viewing permissions".into(),
            );
        }
    }
    if permission == SessionPermission::ReceiveFiles {
        let live = runtime();
        if live["state"] == "running" && live["snapshot"]["file_protocol_version"] != json!(1) {
            return Err("Restart UKVM to load M20 before changing file permissions".into());
        }
    }
    let _lock = edit_lock(&default_trust_dir()?)?;
    let mut store = TrustStore::load(&default_trust_dir()?)?;
    store.set_permission(&default_trust_dir()?, name, permission, action == "allow")?;
    let peer = store.find(name).ok_or("peer disappeared")?;
    Ok(
        json!({"node_id": peer.id, "permissions": peer.permissions, "scope": "this_computer", "apply_required": !matches!(permission,SessionPermission::ShareClipboard|SessionPermission::ReceiveFiles|SessionPermission::ViewScreen), "apply_command": if matches!(permission,SessionPermission::ShareClipboard|SessionPermission::ReceiveFiles|SessionPermission::ViewScreen) { Value::Null } else { json!("ukvm apply") }}),
    )
}

pub(crate) fn apply_configuration() -> Result<Value, String> {
    let _lock = edit_lock(&default_trust_dir()?)?;
    let (config, store) = context()?;
    for node in config.layout.topology() {
        if store.find_id(&node.peer_id).is_none() {
            return Err("layout references an untrusted peer".into());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let state = super::service::properties()?;
        if state.get("LoadState").map(String::as_str) != Some("loaded") {
            return Err("install the user service first: ukvm service install".into());
        }
        let managed_pid = state
            .get("MainPID")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|pid| *pid > 0);
        match ukvm_runtime::ownership::status()? {
            ukvm_runtime::ownership::RuntimeStatus::Stopped => {},
            ukvm_runtime::ownership::RuntimeStatus::Running(snapshot) if Some(snapshot.pid) == managed_pid => {},
            _ => return Err("a foreground/unknown runtime owns this profile; inspect ukvm status and stop it before ukvm apply".into()),
        }
        service_command(&["restart".into()])?;
        Ok(
            json!({"applied": true, "mechanism": "user_service_restart", "consent_may_be_required": true}),
        )
    }
    #[cfg(not(target_os = "linux"))]
    Err("apply currently requires Linux/systemd user service".into())
}

fn service_command(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        if args == ["status"] {
            return Ok(service());
        }
        match args {
            [action] if matches!(action.as_str(), "install" | "enable" | "disable" | "start" | "stop" | "restart") => {},
            [action, flag] if action == "install" && flag == "--verbose" => {},
            _ => return Err("usage: ukvm service install [--verbose]|enable|disable|start|stop|restart|status --json".into()),
        }
        let output =
            std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .arg("service")
                .args(args)
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "service {} failed: {}",
                args[0],
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(json!({"action": args[0], "service": service()}))
    }
    #[cfg(not(target_os = "linux"))]
    Err("service management currently requires Linux".into())
}

fn print_human(command: &str, data: &Value) {
    if command == "authorization" {
        if data["reset"] == true {
            println!("Remembered desktop authorization forgotten. UKVM remains stopped; start it to request normal consent. Peer trust and control permissions are unchanged.");
        }
        super::authorization::print_status(data.get("desktop_authorization").unwrap_or(data));
    } else if command == "layout" {
        let layout = if data.get("layout").is_some() {
            &data["layout"]
        } else {
            data
        };
        println!("UniversalKVM Layout (local computer at 0,0)");
        if let Some(nodes) = layout["nodes"].as_array() {
            for node in nodes {
                println!(
                    "  ({},{}): {} [{}]{}",
                    node["x"],
                    node["y"],
                    node["name"].as_str().unwrap_or("UNTRUSTED"),
                    node["node_id"].as_str().unwrap_or("not initialized"),
                    if node["local"] == true {
                        " (local)"
                    } else {
                        ""
                    }
                );
            }
        }
        if layout["valid"] == false {
            println!("Warning: layout references missing/untrusted peers.");
        }
        if data["apply_required"] == true {
            println!("Saved. Run `ukvm apply` to use the new layout.");
        }
    } else if command == "apply" {
        println!("Configuration applied through the user service. Normal portal consent may be required.");
    } else {
        println!("Unpaired {} locally. Remote trust is unchanged. Run `ukvm apply` to refresh outgoing topology.", data["name"].as_str().unwrap_or("peer"));
    }
}

fn authorization_command(args: &[String]) -> Result<Value, String> {
    if args.is_empty() || args == ["status"] {
        return Ok(super::authorization::status(None, None));
    }
    let yes = match args {
        [action] if action == "reset" => false,
        [action, flag] if action == "reset" && flag == "--yes" => true,
        _ => return Err("usage: ukvm authorization [status|reset [--yes]] [--json]".into()),
    };
    confirm("Stop UKVM and forget remembered desktop input and screen-sharing authorization? Normal portal approval will be required again; peer trust/permissions are unchanged.", yes)?;
    #[cfg(target_os = "linux")]
    {
        use ukvm_runtime::ownership::{self, RuntimeStatus};
        let mut stopped = false;
        match ownership::status()? {
            RuntimeStatus::Stopped => {}
            RuntimeStatus::Running(snapshot) => {
                let properties = super::service::properties()?;
                let pid = properties
                    .get("MainPID")
                    .and_then(|v| v.parse::<u32>().ok());
                if pid != Some(snapshot.pid) {
                    return Err(
                        "stop the foreground runtime before resetting desktop authorization".into(),
                    );
                }
                service_command(&["stop".into()])?;
                stopped = true;
                if !matches!(ownership::status()?, RuntimeStatus::Stopped) {
                    return Err(
                        "UKVM has not finished stopping; retry authorization reset after shutdown"
                            .into(),
                    );
                }
            }
            _ => return Err(
                "runtime ownership is uncertain; stop UKVM before resetting desktop authorization"
                    .into(),
            ),
        }
        ukvm_platform::linux_wayland::authorization::reset().map_err(|error| {
            if stopped {
                format!("UKVM was stopped, but authorization reset failed: {error}")
            } else {
                error
            }
        })?;
        Ok(
            json!({"reset": true, "service_stopped": stopped, "service_restarted": false,
            "trust_changed": false, "control_permissions_changed": false,
            "desktop_authorization": super::authorization::status(None, None)}),
        )
    }
    #[cfg(not(target_os = "linux"))]
    Err("desktop portal authorization currently requires Linux".into())
}
