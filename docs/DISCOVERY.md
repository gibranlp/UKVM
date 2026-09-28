# LAN discovery

UKVM uses DNS-SD over multicast DNS to find **temporary pairing endpoints** on the local link. The provisional service type is `_ukvm._tcp.local.`. It is syntactically valid but is not yet an IANA-registered service name. Discovery is a convenience layer; it does not establish identity or change trust, permissions, addresses in the trusted-peer store, or layout.

## Mechanism and scope

`ukvm-discovery` uses `mdns-sd` 0.21.4, a Rust DNS-SD publisher/browser with Linux, macOS and Windows support. The alternative of native Avahi/Bonjour wrappers would add OS-specific integration at this stage; custom UDP broadcast would add a new protocol. The crate exposes a short-lived `DiscoveryService` with advertisement and bounded browsing. The CLI provides `ukvm discover` and `ukvm pair --discover`; it passes the chosen resolved socket address into the **same Pairing Protocol 2 initiator** as `ukvm pair HOST`. The normal mTLS/input protocol 1.1 is unchanged.

Only `ukvm pair` as a listener advertises, for its existing 90-second pairing window. Exit, timeout, rejection and normal completion drop the advertisement and request DNS-SD goodbye records. A crash or peer cache may leave a stale entry until its DNS TTL expires; connecting to such an entry fails safely. `ukvm discover` browses for four seconds, then stops. There is no persistent discovery cache or daemon.

`mdns-sd::ServiceDaemon::register()` queues registration; it does not confirm a network announcement. UKVM now waits up to five seconds for the daemon's `Announce` event before printing “Discoverable.” An asynchronous daemon error or an announcement timeout prints a warning and leaves direct TCP pairing available. The daemon and advertisement guard remain alive through the pairing listener and ceremony. `ukvm pair --debug` prints the service name, instance, host, candidate local interface addresses, selected interface, ports, and the daemon's announcement target. `ukvm pair --interface NAME --debug` restricts and diagnoses a particular interface. The interface-address list is a local diagnostic snapshot; the daemon chooses the actual multicast addresses at send time.

The advertisement carries:

| DNS-SD field | Value | Security meaning |
| --- | --- | --- |
| Service instance | `ukvm-` plus 16 hex digits from node ID | Untrusted deduplication key; also exposes a stable public identity hint while pairing is active. |
| SRV port | Pairing TCP port, normally 24801 | Connection hint. |
| Address records | Local interface IPv4/IPv6 addresses | Connection hints. |
| TXT `name` | Configured display name, or hostname fallback | Untrusted label. |
| TXT `pair` | Pairing protocol version, currently 2 | Compatibility hint; handshake checks again. |
| TXT `input` | Input protocol version, currently 1.1 | Informational hint. |
| TXT `id` | Full public certificate SHA-256 node ID | **Advertised node ID**, never a verified identity. |
| TXT `control` | Normal UKVM TCP port, normally 24800 | Informational hint, not a listening claim. |

No private key, verification code, pairing secret, permission, layout, or full configuration is advertised. Entries are parsed with size and type checks. The in-memory browse cache is capped at 64 service instances, 16 addresses per instance, and 10 seconds. Repeated resolutions of one service instance merge addresses; matching names or claimed IDs on **different** instances stay separate. A malicious advertiser can still spoof or occupy entries. The displayed `advertised_node_id` is never passed to the trust store. An already trusted looking ID would likewise remain an unverified hint; the CLI does not currently label it “already trusted.”

Default discovery disables loopback and uses the library's eligible LAN interfaces. It may include Ethernet, Wi-Fi, VPN or virtual bridges if those interfaces support multicast. Use `--interface NAME` on either command to restrict browsing or advertising to one named interface. IPv4 and IPv6 results are kept, including IPv6 scope IDs; the initiator tries IPv4 first and then other resolved addresses. Discovery normally stays on the local link and may not cross VLANs, routed networks or VPNs. Direct `ukvm pair HOST` remains supported wherever TCP is reachable.

## Security boundary

Assume the LAN can spoof names, addresses, service instances, versions and advertised node IDs. Selection only chooses where to start Pairing Protocol 2. The existing fresh Noise exchange, public certificate and P-256 identity proof, transcript-bound code, explicit comparison on both machines, two confirmations, and safe trust-store commit remain mandatory. Discovery never grants DebugSink or control permission. A malicious endpoint may consume the pairing attempt or cause a denial of service; it cannot become trusted unless users confirm a matching code for that exchange.

## Network troubleshooting

mDNS uses multicast UDP **5353** (`224.0.0.251` for IPv4 and `ff02::fb` for IPv6). Pairing then uses TCP **24801**; normal authenticated UKVM traffic uses TCP **24800**. UKVM does not change firewall settings. If no node appears, verify that B is actively running `ukvm pair`, both nodes share a multicast-capable link, the selected interface is correct, and local firewalls permit mDNS. `ukvm doctor` reports the compiled discovery backend without scanning the LAN or proving firewall reachability. An incompatible advertised pairing version is shown but cannot be selected; the actual pairing handshake rejects an incompatible version independently.

For the first physical Milestone 10 trial, A's `_ukvm._tcp.local.` PTR queries reached B, but A received no packet from B. B also printed `usage: ukvm pair [HOST[:PORT]] or ukvm pair --bind ADDR` for `pair --interface`. That string belongs to the **pre-Milestone-10** binary, not the Milestone 10 source. This is evidence that the executable used for that CLI check was stale; the absence of B's response is consistent with B running a pairing listener that never registers mDNS. Rebuild and check the exact executable on **both** machines before attributing the remaining failure to the network. `target/debug/ukvm version` now includes `discovery mDNS/DNS-SD`, and `target/debug/ukvm pair --help` lists `--interface` and `--debug`. The later rebuilt-binary physical test succeeded: A discovered B across the LAN.

The local Linux test now runs advertiser and browser in separate OS processes using UDP 5353 and finds the service. It also verifies that registration on a nonexistent interface fails visibly. A same-host result does not prove traffic between A and B. `mdns-sd` opens reusable multicast sockets and reports socket/interface failures via daemon events; UKVM watches those errors and waits for a real announcement. It does not install Avahi or modify the firewall.

## Evidence and physical trial

Deterministic tests cover advertisement fields, malformed records, version hints, duplicate and same-name entries, address handoff, expiry and removal. An optional local multicast smoke test passed. Two isolated XDG profiles on one Linux machine completed `ukvm pair` → `ukvm discover` → `ukvm pair --discover` without a typed IP: the codes matched, reciprocal pins were stored, and input permissions stayed denied. A fresh browse after listener timeout and after successful completion found no advertisement. Discovery has since been physically proven across the two PCs. The isolated test remains distinct from that physical proof.

Use the same tested revision on both machines. In each repository run `git pull --ff-only origin main`, `cargo build -p ukvm`, and `target/debug/ukvm version` before the trial. Confirm that the version line contains `discovery mDNS/DNS-SD`. In **separate temporary shells** on PC B (`192.168.1.40`) and PC A (`192.168.1.21`), run:

```sh
UKVM_M10_PROFILE="$(mktemp -d)"
export XDG_CONFIG_HOME="$UKVM_M10_PROFILE/config"
export XDG_STATE_HOME="$UKVM_M10_PROFILE/state"
export XDG_DATA_HOME="$UKVM_M10_PROFILE/data"
mkdir -p "$XDG_CONFIG_HOME/ukvm"
```

On B:

```sh
printf '[node]\nname = "io"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
target/debug/ukvm identity
target/debug/ukvm pair --interface enp0s20u2u1u2 --debug
```

While B is waiting, on A:

```sh
printf '[node]\nname = "spectrum"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
target/debug/ukvm identity
target/debug/ukvm discover --interface enp10s0
target/debug/ukvm pair --discover --interface enp10s0
```

Select B by its displayed address and name; **compare the codes on both physical screens** and answer yes only if they match. After success, run `target/debug/ukvm peer list`, `target/debug/ukvm peer show <alias>`, and `target/debug/ukvm peer permissions <alias>` on both hosts. Check reciprocal full fingerprints and `connect=true`, `debug-input=false`, `control-this-node=false`. These temporary XDG profiles keep the known-good trust stores untouched. Do not delete the temporary profiles until the result is recorded; each contains a private identity.

For packet inspection, on B capture `udp port 5353` on `enp0s20u2u1u2`. A successful advertiser should send an mDNS announcement from `192.168.1.40` containing `_ukvm._tcp.local.` PTR and associated SRV/TXT/address records. When A browses, B should receive A's PTR query and send an mDNS response from `192.168.1.40`; A's capture on `enp10s0` should see that response. The response may be multicast or unicast according to mDNS behavior, so filter on B's source IP and UDP 5353 rather than only destination `224.0.0.251`.
