//! Ephemeral DNS-SD endpoint hints. Nothing in this crate establishes trust.
use mdns_sd::{
    DaemonEvent, IfKind, Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent,
    ServiceInfo, UnregisterStatus,
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr, SocketAddrV6},
    time::{Duration, Instant},
};

/// Provisional UKVM DNS-SD service type, scoped to the mDNS `.local.` domain.
pub const SERVICE_TYPE: &str = "_ukvm._tcp.local.";
pub const DEFAULT_BROWSE: Duration = Duration::from_secs(4);
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_LIFETIME: Duration = Duration::from_secs(10);
const MAX_CANDIDATES: usize = 64;
const MAX_ADDRESSES: usize = 16;

#[derive(Clone, Debug)]
pub struct Advertisement {
    pub node_name: String,
    pub node_id: String,
    pub pairing_port: u16,
    /// False for a persistent control endpoint; run never enables pairing.
    pub pairing_available: bool,
    pub control_port: u16,
    pub pairing_version: u8,
    pub input_version: (u16, u16),
}

impl Advertisement {
    pub fn service_info(&self) -> Result<ServiceInfo, String> {
        if self.node_name.is_empty()
            || self.node_name.chars().count() > 63
            || self.node_name.chars().any(char::is_control)
            || self.node_id.len() != 64
            || !self.node_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.pairing_port == 0
            || self.control_port == 0
        {
            return Err("invalid discovery advertisement".into());
        }
        // La ceremonia temporal necesita otro nombre DNS-SD para coexistir con el servicio activo.
        let instance = format!(
            "ukvm-{}{}",
            &self.node_id[..16],
            if self.pairing_available { "-pair" } else { "" }
        );
        let hostname = format!("{instance}.local.");
        let name = truncate_utf8(&self.node_name, 120);
        let pair = self.pairing_version.to_string();
        let input = format!("{}.{}", self.input_version.0, self.input_version.1);
        let control = self.control_port.to_string();
        ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &hostname,
            "",
            self.pairing_port,
            &[
                ("name", name),
                (
                    "mode",
                    if self.pairing_available {
                        "pairing"
                    } else {
                        "control"
                    },
                ),
                ("pair", pair.as_str()),
                ("input", input.as_str()),
                ("id", self.node_id.as_str()),
                ("control", control.as_str()),
            ][..],
        )
        .map(ServiceInfo::enable_addr_auto)
        .map_err(|e| format!("discovery advertisement: {e}"))
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryResult {
    pub service_instance: String,
    pub display_name: String,
    /// Untrusted TXT hint. Never use this as the verified peer ID.
    pub advertised_node_id: Option<String>,
    pub pairing_version: u8,
    pub pairing_available: bool,
    pub input_version: Option<(u16, u16)>,
    pub control_port: Option<u16>,
    pub addresses: Vec<SocketAddr>,
}

impl DiscoveryResult {
    pub fn compatible(&self, local_pairing_version: u8) -> bool {
        self.pairing_available && self.pairing_version == local_pairing_version
    }

    /// DNS-SD addresses are connection hints only. Prefer IPv4 for the current
    /// two-PC trial, retaining scoped IPv6 alternatives for future use.
    pub fn pairing_addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
}

fn parse_version(value: &str) -> Option<(u16, u16)> {
    let (major, minor) = value.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn socket_address(value: &ScopedIp, port: u16) -> Option<SocketAddr> {
    let addr = match value {
        ScopedIp::V4(ip) => SocketAddr::new(IpAddr::V4(*ip.addr()), port),
        ScopedIp::V6(ip) => {
            SocketAddr::V6(SocketAddrV6::new(*ip.addr(), port, 0, ip.scope_id().index))
        }
        _ => return None,
    };
    if addr.ip().is_unspecified() || addr.ip().is_multicast() || addr.ip().is_loopback() {
        return None;
    }
    Some(addr)
}

pub fn parse_service(service: &ResolvedService) -> Result<DiscoveryResult, String> {
    if !service.ty_domain.eq_ignore_ascii_case(SERVICE_TYPE)
        || service.fullname.len() > 255
        || service.port == 0
    {
        return Err("invalid UKVM discovery service".into());
    }
    let name = service
        .get_property_val_str("name")
        .ok_or("discovery name missing")?;
    if name.is_empty() || name.chars().count() > 63 || name.chars().any(char::is_control) {
        return Err("invalid discovery name".into());
    }
    let pairing_version = service
        .get_property_val_str("pair")
        .ok_or("pairing version missing")?
        .parse::<u8>()
        .map_err(|_| "invalid pairing version")?;
    let advertised_node_id = service
        .get_property_val_str("id")
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    let input_version = service
        .get_property_val_str("input")
        .and_then(parse_version);
    let control_port = service
        .get_property_val_str("control")
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|p| *p > 0);
    let mut addresses: Vec<_> = service
        .addresses
        .iter()
        .filter_map(|ip| socket_address(ip, service.port))
        .collect();
    addresses.sort_by_key(|addr| (addr.is_ipv6(), addr.to_string()));
    addresses.dedup();
    addresses.truncate(MAX_ADDRESSES);
    if addresses.is_empty() {
        return Err("discovery service has no usable address".into());
    }
    Ok(DiscoveryResult {
        service_instance: service.fullname.clone(),
        display_name: name.into(),
        advertised_node_id,
        pairing_version,
        pairing_available: service
            .get_property_val_str("mode")
            .is_none_or(|mode| mode == "pairing"),
        input_version,
        control_port,
        addresses,
    })
}

struct CacheEntry {
    result: DiscoveryResult,
    seen: Instant,
}

#[derive(Default)]
pub struct DiscoveryCache {
    entries: BTreeMap<String, CacheEntry>,
}

impl DiscoveryCache {
    pub fn observe(&mut self, result: DiscoveryResult, now: Instant) {
        self.expire(now);
        let key = result.service_instance.to_ascii_lowercase();
        if let Some(entry) = self.entries.get_mut(&key) {
            // One instance can be resolved on several interfaces. The hint is
            // still untrusted; do not merge separate instances by claimed ID.
            if entry.result.display_name == result.display_name
                && entry.result.pairing_version == result.pairing_version
                && entry.result.pairing_available == result.pairing_available
                && entry.result.advertised_node_id == result.advertised_node_id
                && entry.result.control_port == result.control_port
                && entry.result.input_version == result.input_version
            {
                entry.result.addresses.extend(result.addresses);
                entry
                    .result
                    .addresses
                    .sort_by_key(|addr| (addr.is_ipv6(), addr.to_string()));
                entry.result.addresses.dedup();
                entry.result.addresses.truncate(MAX_ADDRESSES);
                entry.seen = now;
            } else {
                *entry = CacheEntry { result, seen: now };
            }
        } else if self.entries.len() < MAX_CANDIDATES {
            self.entries.insert(key, CacheEntry { result, seen: now });
        }
    }
    pub fn remove(&mut self, fullname: &str) {
        self.entries.remove(&fullname.to_ascii_lowercase());
    }
    pub fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.seen) < CACHE_LIFETIME);
    }
    pub fn results(mut self, now: Instant) -> Vec<DiscoveryResult> {
        self.expire(now);
        let mut results: Vec<_> = self.entries.into_values().map(|v| v.result).collect();
        results.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then(a.service_instance.cmp(&b.service_instance))
        });
        results
    }
}

/// A short-lived, platform-independent DNS-SD publisher/browser.
pub struct DiscoveryService {
    daemon: ServiceDaemon,
    monitor: Receiver<DaemonEvent>,
}

impl DiscoveryService {
    pub fn new(interface: Option<&str>) -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| format!("mDNS unavailable: {e}"))?;
        configure_interfaces(&daemon, interface)?;
        let monitor = daemon.monitor().map_err(|e| e.to_string())?;
        Ok(Self { daemon, monitor })
    }

    #[cfg(test)]
    fn with_port(port: u16, interface: Option<&str>) -> Result<Self, String> {
        let daemon = ServiceDaemon::new_with_port(port).map_err(|e| e.to_string())?;
        configure_interfaces(&daemon, interface)?;
        let monitor = daemon.monitor().map_err(|e| e.to_string())?;
        Ok(Self { daemon, monitor })
    }

    pub async fn advertise(
        &self,
        advertisement: &Advertisement,
    ) -> Result<AdvertisementGuard, String> {
        let info = advertisement.service_info()?;
        let fullname = info.get_fullname().to_string();
        let hostname = info.get_hostname().to_string();
        self.daemon
            .register(info)
            .map_err(|e| format!("mDNS registration: {e}"))?;
        let mut guard = AdvertisementGuard {
            daemon: self.daemon.clone(),
            fullname: fullname.clone(),
            hostname,
            announcement_target: String::new(),
        };
        // `register` only enqueues a command. An Announce event confirms that
        // the daemon actually sent a packet on at least one interface.
        let announced = tokio::time::timeout(ANNOUNCE_TIMEOUT, async {
            loop {
                match self.monitor.recv_async().await {
                    Ok(DaemonEvent::Announce(service, addresses))
                        if service.eq_ignore_ascii_case(&fullname) =>
                    {
                        return Ok(addresses);
                    }
                    Ok(DaemonEvent::Error(error)) => {
                        return Err(format!("mDNS registration failed: {error}"));
                    }
                    Ok(_) => {}
                    Err(error) => return Err(format!("mDNS daemon stopped: {error}")),
                }
            }
        })
        .await
        .map_err(|_| {
            "mDNS registration did not announce on any interface within five seconds".to_string()
        })??;
        guard.announcement_target = announced;
        Ok(guard)
    }

    pub async fn browse_for(&self, duration: Duration) -> Result<Vec<DiscoveryResult>, String> {
        let events = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| format!("mDNS browse: {e}"))?;
        let mut cache = DiscoveryCache::default();
        let deadline = tokio::time::Instant::now() + duration;
        let result = loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break Ok(cache.results(Instant::now())),
                event = events.recv_async() => match event {
                    Ok(ServiceEvent::ServiceResolved(service)) => if let Ok(candidate) = parse_service(&service) { cache.observe(candidate, Instant::now()); },
                    Ok(ServiceEvent::ServiceRemoved(_, fullname)) => cache.remove(&fullname),
                    Ok(_) => {},
                    Err(e) => break Err(format!("mDNS browse stopped: {e}")),
                },
                event = self.monitor.recv_async() => match event {
                    Ok(DaemonEvent::Error(e)) => break Err(format!("mDNS socket/interface error: {e}")),
                    Ok(_) => {},
                    Err(e) => break Err(format!("mDNS daemon stopped: {e}")),
                }
            }
        };
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        result
    }

    pub fn take_error(&self) -> Option<String> {
        while let Ok(event) = self.monitor.try_recv() {
            if let DaemonEvent::Error(error) = event {
                return Some(error.to_string());
            }
        }
        None
    }
}

fn configure_interfaces(daemon: &ServiceDaemon, interface: Option<&str>) -> Result<(), String> {
    if let Some(name) = interface {
        if name.is_empty()
            || name.len() > 64
            || name.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err("invalid discovery interface name".into());
        }
        daemon
            .disable_interface(IfKind::All)
            .map_err(|e| e.to_string())?;
        daemon
            .enable_interface(IfKind::Name(name.into()))
            .map_err(|e| e.to_string())?;
    } else {
        daemon
            .disable_interface(IfKind::LoopbackV4)
            .map_err(|e| e.to_string())?;
        daemon
            .disable_interface(IfKind::LoopbackV6)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Removes the DNS-SD record on all normal exit paths and requests goodbye
/// packets. A crash can still leave TTL-cached records briefly on neighbors.
pub struct AdvertisementGuard {
    daemon: ServiceDaemon,
    fullname: String,
    hostname: String,
    announcement_target: String,
}

impl AdvertisementGuard {
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
    pub fn hostname(&self) -> &str {
        &self.hostname
    }
    /// The daemon's announcement target (hostname and interface), not a DNS
    /// address record. Use `local_interface_addresses` for local IP details.
    pub fn announcement_target(&self) -> &str {
        &self.announcement_target
    }
    pub fn stop(self) {
        drop(self);
    }
}

/// Diagnostic snapshot of local addresses that may be used for mDNS. The
/// daemon independently selects active multicast interfaces at send time.
pub fn local_interface_addresses(interface: Option<&str>) -> Result<Vec<String>, String> {
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()
        .map_err(|e| format!("cannot inspect local interfaces: {e}"))?
        .into_iter()
        .filter(|item| item.is_oper_up() && !item.is_loopback() && !item.is_p2p())
        .filter(|item| interface.is_none_or(|name| item.name == name))
        .map(|item| format!("{} {}", item.name, item.ip()))
        .collect();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

impl Drop for AdvertisementGuard {
    fn drop(&mut self) {
        if let Ok(status) = self.daemon.unregister(&self.fullname) {
            let _ = status
                .recv_timeout(Duration::from_millis(500))
                .map(|response| matches!(response, UnregisterStatus::OK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(name: &str, id: &str, ip: &str, pair: &str) -> ResolvedService {
        ServiceInfo::new(
            SERVICE_TYPE,
            &format!("ukvm-{}", &id[..id.len().min(16)]),
            "ukvm-test.local.",
            ip,
            24801,
            &[
                ("name", name),
                ("pair", pair),
                ("input", "1.1"),
                ("id", id),
                ("control", "24800"),
            ][..],
        )
        .unwrap()
        .as_resolved_service()
    }

    #[test]
    fn temporary_pairing_and_persistent_runtime_have_distinct_dns_sd_instances() {
        let mut advertisement = Advertisement {
            node_name: "io".into(),
            node_id: "a".repeat(64),
            pairing_port: 24801,
            control_port: 24800,
            pairing_version: 2,
            pairing_available: false,
            input_version: (1, 3),
        };
        let runtime = advertisement.service_info().unwrap();
        assert_eq!(
            runtime.get_fullname(),
            "ukvm-aaaaaaaaaaaaaaaa._ukvm._tcp.local."
        );
        advertisement.pairing_available = true;
        let pairing = advertisement.service_info().unwrap();
        assert_ne!(runtime.get_fullname(), pairing.get_fullname());
        assert_ne!(runtime.get_hostname(), pairing.get_hostname());
        assert_eq!(
            runtime.get_property_val_str("id"),
            pairing.get_property_val_str("id")
        );
        assert_eq!(runtime.get_property_val_str("mode"), Some("control"));
        assert_eq!(pairing.get_property_val_str("mode"), Some("pairing"));
    }

    #[test]
    fn advertisement_roundtrip_and_address_handoff() {
        let id = "a".repeat(64);
        let info = Advertisement {
            node_name: "io".into(),
            node_id: id.clone(),
            pairing_port: 24801,
            control_port: 24800,
            pairing_version: 2,
            pairing_available: true,
            input_version: (1, 1),
        }
        .service_info()
        .unwrap();
        assert!(info.is_addr_auto());
        assert_eq!(info.get_port(), 24801);
        assert_eq!(info.get_property_val_str("name"), Some("io"));
        let candidate =
            parse_service(&advertised("io", &id, "192.168.1.40,2001:db8::40", "2")).unwrap();
        assert_eq!(candidate.advertised_node_id.as_deref(), Some(id.as_str()));
        assert_eq!(candidate.input_version, Some((1, 1)));
        assert_eq!(candidate.control_port, Some(24800));
        assert_eq!(
            candidate.addresses[0],
            "192.168.1.40:24801".parse().unwrap()
        );
        assert!(candidate.addresses[1].is_ipv6());
        assert!(candidate.compatible(2));
    }

    #[test]
    fn persistent_control_advertisement_is_not_a_pairing_endpoint() {
        let advertisement = Advertisement {
            node_name: "runtime".into(),
            node_id: "d".repeat(64),
            pairing_port: 24800,
            pairing_available: false,
            control_port: 24800,
            pairing_version: 2,
            input_version: (1, 2),
        };
        let info = advertisement.service_info().unwrap();
        assert_eq!(info.get_property_val_str("mode"), Some("control"));
        let mut service = advertised("runtime", &advertisement.node_id, "192.168.1.40", "2");
        service.txt_properties = info.get_properties().clone();
        let candidate = parse_service(&service).unwrap();
        assert!(!candidate.pairing_available);
        assert!(!candidate.compatible(2));
        assert_eq!(candidate.control_port, Some(24800));
    }

    #[test]
    fn malformed_missing_and_old_versions_do_not_gain_trust() {
        let id = "b".repeat(64);
        let old = parse_service(&advertised("old", &id, "192.168.1.40", "1")).unwrap();
        assert!(!old.compatible(2));
        let invalid = advertised("io", &id, "192.168.1.40", "bad");
        assert!(parse_service(&invalid).is_err());
        let missing = ServiceInfo::new(
            SERVICE_TYPE,
            "missing",
            "missing.local.",
            "192.168.1.40",
            24801,
            &[("name", "io")][..],
        )
        .unwrap()
        .as_resolved_service();
        assert!(parse_service(&missing).is_err());
        let spoof = advertised("io", "c", "192.168.1.40", "2");
        let parsed = parse_service(&spoof).unwrap();
        assert!(parsed.advertised_node_id.is_none());
        assert!(Advertisement {
            node_name: "".into(),
            node_id: id,
            pairing_port: 24801,
            control_port: 24800,
            pairing_version: 2,
            pairing_available: true,
            input_version: (1, 1)
        }
        .service_info()
        .is_err());
    }

    fn result(name: &str, instance: &str, id: &str, address: &str) -> DiscoveryResult {
        DiscoveryResult {
            service_instance: instance.into(),
            display_name: name.into(),
            advertised_node_id: Some(id.into()),
            pairing_version: 2,
            pairing_available: true,
            input_version: Some((1, 1)),
            control_port: Some(24800),
            addresses: vec![address.parse().unwrap()],
        }
    }

    #[test]
    fn cache_deduplicates_instances_not_untrusted_ids_or_names() {
        let now = Instant::now();
        let mut cache = DiscoveryCache::default();
        cache.observe(
            result(
                "desktop",
                "one._ukvm._tcp.local.",
                "claimed",
                "192.168.1.40:24801",
            ),
            now,
        );
        cache.observe(
            result(
                "desktop",
                "one._ukvm._tcp.local.",
                "claimed",
                "192.168.1.41:24801",
            ),
            now,
        );
        cache.observe(
            result(
                "desktop",
                "two._ukvm._tcp.local.",
                "claimed",
                "192.168.1.52:24801",
            ),
            now,
        );
        let results = cache.results(now);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].addresses.len(), 2);
        assert_eq!(results[1].addresses.len(), 1);
        let mut cache = DiscoveryCache::default();
        cache.observe(result("io", "one", "id", "192.168.1.40:24801"), now);
        cache.remove("one");
        assert!(cache.results(now).is_empty());
        let mut cache = DiscoveryCache::default();
        cache.observe(result("io", "one", "id", "192.168.1.40:24801"), now);
        assert!(cache.results(now + CACHE_LIFETIME).is_empty());
    }

    #[tokio::test]
    #[ignore = "requires working multicast on a local interface"]
    async fn optional_live_multicast_smoke() {
        let service = DiscoveryService::with_port(53539, None).unwrap();
        let advertisement = Advertisement {
            node_name: "ukvm-smoke".into(),
            node_id: "d".repeat(64),
            pairing_port: 24801,
            control_port: 24800,
            pairing_version: 2,
            pairing_available: true,
            input_version: (1, 1),
        };
        let guard = service.advertise(&advertisement).await.unwrap();
        let found = service.browse_for(Duration::from_secs(3)).await.unwrap();
        assert!(found.iter().any(|c| c.display_name == "ukvm-smoke"));
        guard.stop();
    }
}
