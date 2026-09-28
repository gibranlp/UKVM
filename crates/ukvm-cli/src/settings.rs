use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};
use ukvm_config::{Config, Edge};
use ukvm_network::{
    identity::{certificate_id, default_state_dir},
    trust::{default_trust_dir, resolve_address, SessionPermission, TrustStore},
};

/// Serialize read-modify-write CLI edits across processes. The OS releases
/// the advisory lock even if a command exits unexpectedly.
fn edit_lock() -> Result<std::fs::File, String> {
    let dir = default_trust_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(dir.join(".edit.lock"))
        .map_err(|e| e.to_string())?;
    fs2::FileExt::lock_exclusive(&file).map_err(|e| format!("cannot lock UKVM edits: {e}"))?;
    Ok(file)
}

fn paths() -> Result<(PathBuf, PathBuf), String> {
    Ok((
        ukvm_config::default_path().map_err(|e| e.to_string())?,
        default_trust_dir()?,
    ))
}
fn context() -> Result<(PathBuf, PathBuf, Config, TrustStore), String> {
    let (config_path, state) = paths()?;
    let config = ukvm_config::load_or_default(&config_path).map_err(|e| e.to_string())?;
    let store = TrustStore::load(&state)?;
    Ok((config_path, state, config, store))
}
fn validated() -> Result<(PathBuf, PathBuf, Config, TrustStore), String> {
    let context = context()?;
    for link in &context.2.layout.links {
        let peer = context.3.find_id(&link.peer_id).ok_or_else(|| {
            format!(
                "{} edge references unknown peer {}",
                link.edge.as_str(),
                link.peer_id
            )
        })?;
        if peer.address.is_none() {
            return Err(format!(
                "{} edge peer {} has no address",
                link.edge.as_str(),
                peer.alias
            ));
        }
    }
    let local_cert = default_state_dir()?.join("node.der");
    if local_cert.exists() {
        let bytes = std::fs::read(local_cert).map_err(|e| e.to_string())?;
        if context.3.find_id(&certificate_id(&bytes)).is_some() {
            return Err("this node is listed as its own peer".into());
        }
    }
    Ok(context)
}
fn permission(value: &str) -> Result<SessionPermission, String> {
    match value {
        "connect" => Ok(SessionPermission::Connect),
        "debug-input" => Ok(SessionPermission::DebugInput),
        "control-this-node" => Ok(SessionPermission::ControlThisNode),
        _ => Err("permission must be connect, debug-input, or control-this-node".into()),
    }
}
fn confirmation() -> Result<bool, String> {
    print!("Add this peer? [y/N] ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes"))
}

pub fn peer(args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), String> {
        let _lock = edit_lock()?;
        let (_, state, _, mut store) = context()?;
        match args {
            [command] if command == "list" => { if store.peers.is_empty() { println!("No trusted peers."); } for peer in &store.peers { println!("{}  {}  {}",peer.alias,peer.id,peer.address.as_deref().unwrap_or("no address")); } }
            [command, name] if command == "show" || command == "permissions" => {
                let peer = store.find(name).ok_or("unknown peer")?;
                println!("Peer: {}\nNode ID / SHA-256 fingerprint: {}\nAddress: {}\nCertificate: {}\nPermissions:\n  connect: {}\n  debug-input: {}\n  control-this-node: {}",peer.alias,peer.id,peer.address.as_deref().unwrap_or("none"),ukvm_network::trust::cert_path(&state,&peer.id).display(),peer.permissions.connect,peer.permissions.debug_input,peer.permissions.control_this_node);
            }
            [command, name] if command == "remove" => {
                let peer = store.find(name).ok_or("unknown peer")?.clone();
                let (config_path, _, mut config, _) = context()?;
                config.layout.links.retain(|link| link.peer_id != peer.id);
                ukvm_config::save(&config_path,&config).map_err(|e| e.to_string())?;
                store.remove(&state,name)?; println!("Removed peer {} and its layout links.",peer.alias);
            }
            [command, name, permission_name] if command == "allow" || command == "deny" => { let p=permission(permission_name)?; store.set_permission(&state,name,p,command == "allow")?; println!("{} {} for {}",if command == "allow" { "Allowed" } else { "Denied" },permission_name,name); }
            [command, cert_path, rest @ ..] if command == "add" => {
                let mut name=None; let mut address=None; let mut it=rest.iter();
                while let Some(flag)=it.next() { let value=it.next().ok_or_else(||format!("{flag} needs a value"))?; match flag.as_str() { "--name" if name.is_none() => name=Some(value.clone()), "--address" if address.is_none() => address=Some(value.clone()), _ => return Err("usage: ukvm peer add CERT --name ALIAS [--address HOST[:PORT]]".into()) } }
                let name=name.ok_or("--name ALIAS is required")?;
                if let Some(a)=&address { resolve_address(a)?; }
                let cert=std::fs::read(cert_path).map_err(|e| format!("certificate: {e}"))?;
                if cert.is_empty() || cert.len()>4096 { return Err("certificate length is invalid".into()); }
                let id=certificate_id(&cert);
                if store.find(&id).is_some() || store.find(&name).is_some() { return Err("peer ID or alias already exists".into()); }
                let local=default_state_dir()?.join("node.der"); if local.exists() && certificate_id(&std::fs::read(local).map_err(|e|e.to_string())?)==id { return Err("cannot add this node as a peer".into()); }
                println!("Peer certificate\n  Name: {name}\n  Node ID / SHA-256 fingerprint: {id}\n  Address: {}\n  Initial permissions: connect allowed; debug-input denied; control-this-node denied",address.as_deref().unwrap_or("none"));
                if !confirmation()? { println!("Cancelled."); return Ok(()); }
                store.import(&state,name.clone(),address,&cert)?; println!("Peer '{name}' added.");
            }
            _ => return Err("usage: ukvm peer list|show NAME|permissions NAME|add CERT --name ALIAS [--address HOST]|remove NAME|allow NAME PERMISSION|deny NAME PERMISSION".into()),
        }
        Ok(())
    })();
    finish(result)
}

pub fn layout(args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), String> {
        let _lock = edit_lock()?;
        let (path, _, mut config, store) = context()?;
        match args {
            [] => {
                println!("UniversalKVM Layout\nThis node: {}", config.node.name);
                for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                    if let Some(link) = config.layout.links.iter().find(|l| l.edge == edge) {
                        let peer = store
                            .find_id(&link.peer_id)
                            .ok_or("layout references unknown peer")?;
                        println!(
                            "{}: {} ({})",
                            edge.as_str().to_uppercase(),
                            peer.alias,
                            peer.address.as_deref().unwrap_or("no address")
                        );
                    } else {
                        println!("{}: none", edge.as_str().to_uppercase());
                    }
                }
            }
            [command, edge, name] if command == "set" => {
                let edge = Edge::parse(edge)?;
                let peer = store.find(name).ok_or("unknown peer")?;
                if peer.address.is_none() {
                    return Err("peer needs an address before being placed in layout".into());
                }
                config.set_link(edge, peer.id.clone());
                ukvm_config::save(&path, &config).map_err(|e| e.to_string())?;
                println!("{}: {}", edge.as_str(), peer.alias);
            }
            [command, edge] if command == "remove" => {
                let edge = Edge::parse(edge)?;
                if !config.remove_link(edge) {
                    return Err("no peer on that edge".into());
                }
                ukvm_config::save(&path, &config).map_err(|e| e.to_string())?;
                println!("Removed {} layout link.", edge.as_str());
            }
            _ => return Err("usage: ukvm layout [set EDGE PEER|remove EDGE]".into()),
        }
        Ok(())
    })();
    finish(result)
}

pub fn config(args: &[String]) -> ExitCode {
    let result = (|| -> Result<(), String> {
        match args {
            [] => {
                let (path, state) = paths()?;
                println!(
                    "Configuration: {}\nTrust state: {}\nIdentity: {}",
                    path.display(),
                    state.display(),
                    default_state_dir()?.join("node.key").display()
                );
            }
            [command] if command == "path" => {
                let (path, state) = paths()?;
                println!(
                    "Configuration: {}\nTrust state: {}\nIdentity: {}",
                    path.display(),
                    state.display(),
                    default_state_dir()?.join("node.key").display()
                );
            }
            [command] if command == "check" => {
                validated()?;
                println!("Configuration, trust records, certificates, and layout: valid");
            }
            [command] if command == "show" => {
                let (_, _, c, _) = validated()?;
                println!("{}", toml::to_string_pretty(&c).map_err(|e| e.to_string())?);
            }
            _ => return Err("usage: ukvm config [path|check|show]".into()),
        }
        Ok(())
    })();
    finish(result)
}

pub fn status() -> ExitCode {
    let result = (|| -> Result<(), String> {
        let (_, _, config, store) = validated()?;
        let id = match std::fs::read(default_state_dir()?.join("node.der")) {
            Ok(bytes) => certificate_id(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                "not initialized; run ukvm identity".into()
            }
            Err(e) => return Err(e.to_string()),
        };
        println!("UniversalKVM\nNode\n  Name: {}\n  ID: {}\nPlatform\n  OS: {}\nNetwork\n  Listen: {}\nTrusted peers: {}",config.node.name,id,std::env::consts::OS,config.network.listen,store.peers.len());
        for peer in &store.peers {
            let edges = config
                .layout
                .links
                .iter()
                .filter(|l| l.peer_id == peer.id)
                .map(|l| l.edge.as_str())
                .collect::<Vec<_>>();
            println!(
                "  {}: {}  position: {}  debug-input: {}  control-this-node: {}",
                peer.alias,
                peer.address.as_deref().unwrap_or("no address"),
                if edges.is_empty() {
                    "none".into()
                } else {
                    edges.join(",")
                },
                peer.permissions.debug_input,
                peer.permissions.control_this_node
            );
        }
        #[cfg(target_os = "linux")]
        {
            let probe = super::with_runtime(ukvm_platform::linux_wayland::probe());
            println!("Capabilities\n  Controller pointer: {}\n  Controller keyboard: {}\n  Controlled pointer: false\n  Reason: {}",probe.capture.pointer_advertised,probe.capture.keyboard_advertised,if probe.remote.version.is_some() { "UKVM Wayland injector not implemented" } else { "RemoteDesktop portal unavailable" });
        }
        println!("Configuration: valid");
        Ok(())
    })();
    finish(result)
}
fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => super::fail(&e),
    }
}
