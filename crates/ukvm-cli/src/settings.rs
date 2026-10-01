use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};
use ukvm_config::{Config, Edge};
use ukvm_network::{
    identity::{certificate_id, default_state_dir},
    trust::{default_trust_dir, edit_lock, resolve_address, SessionPermission, TrustStore},
};

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
    for node in context.2.layout.topology() {
        context.3.find_id(&node.peer_id).ok_or_else(|| {
            format!(
                "coordinate {} references unknown peer {}",
                node.coordinate(),
                node.peer_id
            )
        })?;
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
        let _lock = edit_lock(&default_trust_dir()?)?;
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
                config.remove_peer_from_layout(&peer.id);
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
        let _lock = edit_lock(&default_trust_dir()?)?;
        let (path, _, mut config, store) = context()?;
        match args {
            [] => {
                println!("UniversalKVM Layout\nThis node: {}", config.node.name);
                for node in config.layout.topology() {
                    let peer = store
                        .find_id(&node.peer_id)
                        .ok_or("layout references unknown peer")?;
                    println!(
                        "{}: {} [{}] ({})",
                        node.coordinate(),
                        peer.alias,
                        peer.id,
                        peer.address.as_deref().unwrap_or("discovery only")
                    );
                }
            }
            [command, x, y, name] if command == "place" => {
                let x = x.parse().map_err(|_| "x must be an integer")?;
                let y = y.parse().map_err(|_| "y must be an integer")?;
                let peer = store.find(name).ok_or("unknown trusted peer")?;
                let local_cert = default_state_dir()?.join("node.der");
                if local_cert.exists()
                    && certificate_id(&std::fs::read(local_cert).map_err(|e| e.to_string())?)
                        == peer.id
                {
                    return Err("cannot place this node as its own remote peer".into());
                }
                config
                    .set_position(x, y, peer.id.clone())
                    .map_err(|e| e.to_string())?;
                ukvm_config::save(&path, &config).map_err(|e| e.to_string())?;
                println!("({x},{y}): {} [{}]", peer.alias, peer.id);
            }
            [command, x, y] if command == "remove-position" => {
                let x = x.parse().map_err(|_| "x must be an integer")?;
                let y = y.parse().map_err(|_| "y must be an integer")?;
                config.remove_position(x, y).map_err(|e| e.to_string())?;
                ukvm_config::save(&path, &config).map_err(|e| e.to_string())?;
                println!("Removed ({x},{y}).");
            }
            [command, edge, name] if command == "set" => {
                let edge = Edge::parse(edge)?;
                let peer = store.find(name).ok_or("unknown peer")?;
                if peer.address.is_none() {
                    return Err("peer needs an address before being placed in layout".into());
                }
                let local_cert = default_state_dir()?.join("node.der");
                if local_cert.exists()
                    && certificate_id(&std::fs::read(local_cert).map_err(|e| e.to_string())?)
                        == peer.id
                {
                    return Err("cannot link this node to itself".into());
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
            _ => return Err(
                "usage: ukvm layout [place X Y PEER|remove-position X Y|set EDGE PEER|remove EDGE]"
                    .into(),
            ),
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

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => super::fail(&e),
    }
}
