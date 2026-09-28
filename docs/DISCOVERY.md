# LAN discovery

UKVM uses DNS-SD over multicast DNS to find **temporary pairing endpoints** on the local link. The provisional service type is `_ukvm._tcp.local.`. It is syntactically valid but is not yet an IANA-registered service name. Discovery is a convenience layer; it does not establish identity or change trust, permissions, addresses in the trusted-peer store, or layout.

## Mechanism and scope

`ukvm-discovery` uses `mdns-sd` 0.21.4, a Rust DNS-SD publisher/browser with Linux, macOS and Windows support. The alternative of native Avahi/Bonjour wrappers would add OS-specific integration at this stage; custom UDP broadcast would add a new protocol. The crate exposes a short-lived `DiscoveryService` with advertisement and bounded browsing. The CLI provides `ukvm discover` and `ukvm pair --discover`; it passes the chosen resolved socket address into the **same Pairing Protocol 2 initiator** as `ukvm pair HOST`. The normal mTLS/input protocol 1.1 is unchanged.

Only `ukvm pair` as a listener advertises, for its existing 90-second pairing window. Exit, timeout, rejection and normal completion drop the advertisement and request DNS-SD goodbye records. A crash or peer cache may leave a stale entry until its DNS TTL expires; connecting to such an entry fails safely. `ukvm discover` browses for four seconds, then stops. There is no persistent discovery cache or daemon.

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

## Evidence and physical trial

Deterministic tests cover advertisement fields, malformed records, version hints, duplicate and same-name entries, address handoff, expiry and removal. An optional local multicast smoke test passed. Two isolated XDG profiles on one Linux machine completed `ukvm pair` → `ukvm discover` → `ukvm pair --discover` without a typed IP: the codes matched, reciprocal pins were stored, and input permissions stayed denied. A fresh browse after listener timeout and after successful completion found no advertisement. This does **not** prove discovery across two physical hosts; the PC A/B trial below remains outstanding.

Use the same tested revision on both machines. In **separate temporary shells** on PC B (`192.168.1.40`) and PC A (`192.168.1.21`), run:

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
ukvm identity
ukvm pair
```

While B is waiting, on A:

```sh
printf '[node]\nname = "spectrum"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
ukvm identity
ukvm discover
ukvm pair --discover
```

Select B by its displayed address and name; **compare the codes on both physical screens** and answer yes only if they match. After success, run `ukvm peer list`, `ukvm peer show <alias>`, and `ukvm peer permissions <alias>` on both hosts. Check reciprocal full fingerprints and `connect=true`, `debug-input=false`, `control-this-node=false`. These temporary XDG profiles keep the known-good trust stores untouched. Do not delete the temporary profiles until the result is recorded; each contains a private identity.
