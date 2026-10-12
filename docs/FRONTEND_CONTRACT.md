# Linux management CLI contract — schema version 1

M16 adds Linux configuration and management UX over the existing M15 runtime. A frontend invokes the installed `ukvm` executable as the logged-in user with an argument array, inherits that user's graphical/XDG environment, and parses JSON. It never edits config, certificates, trust, runtime snapshots or systemd files itself. UKVM has no SpectrumOS dependency. M18 adds local-only initialization and backend classification without changing trust, pairing or input protocols.

## Framing, compatibility and errors

Normal structured commands write exactly one UTF-8 JSON object plus newline to stdout:

```json
{"schema_version":1,"command":"peers","ok":true,"data":{"peers":[]},"error":null}
```

Exit 0 means the requested operation/query succeeded; exit 1 means failure. On failure, `ok` is false, `data` is null, and `error` is `{ "code": "operation_failed", "message": "user-visible explanation" }`. Missing services/buses and unhealthy runtimes are also represented in successful status payloads; `ok` alone never means input control is ready. Invalid config/trust prevents a reliable query and returns an error envelope. stderr is diagnostic text, never part of the JSON protocol. Do not parse human messages or use them as machine discriminators. Signals, an unavailable executable, or process termination before a result can leave no complete result; the caller must handle this as an interrupted operation, not success.

Version 1 permits additive fields; ignore unknown fields and handle unknown enum values as unknown/unavailable. Breaking changes require a new `schema_version`. Names are labels; full Node IDs are identity selectors. Null means unknown/not applicable, never false or permission granted. Availability is sampled and advisory. JSON strings are untrusted display text: render as plain text, never markup or shell commands. Keep mutations serialized per profile, re-query after each operation, and invoke processes without a shell. `docs/cli-schema-v1.json` specifies the envelope and core payloads.

## Supported commands

| Purpose | Argument array after `ukvm` | Result data |
| --- | --- | --- |
| Local initialization | `setup --local [--name NAME] --json` | `node`, `initialized`, `configuration_changed`, `apply_required`, `apply_command`, `consent_requested: false`, `service_started: false`, `trust_changed: false` |
| Health/config/runtime | `status --json` | `node`, `runtime`, `service`, `peers`, `layout`, `health`, `configuration`, `limitations` |
| Trusted machines | `peers --json` | `peers` array |
| LAN discovery | `discover --json [--interface NAME]` | `machines` array, `browse_seconds` (4) |
| Portal capabilities | `doctor --json` | `os`, `session`, `portal`, `input_capture`, `remote_desktop`, `connect_to_eis_available`, `consent_requested`, `authorization`, `physical_input`, `limitations` |
| Read topology | `layout [show] --json` | `nodes`, `issues`, `valid`, `coordinate_model`, `apply_command` |
| Place trusted peer | `layout place X Y PEER --json` | `changed`, `layout`, `apply_required`, `apply_command` |
| Move placed peer | `layout move PEER X Y --json` | Same as place |
| Remove placement | `layout remove PEER --yes --json` | Same as place |
| Remove coordinate | `layout remove-position X Y --yes --json` | Same as place |
| Inspect permission | `peer permissions PEER --json` | `node_id`, `name`, `permissions`, `scope` |
| Grant/revoke permission | `peer allow PEER control-this-node --json` / `peer deny PEER control-this-node --json` | `node_id`, `permissions`, `scope`, `apply_command` |
| Unpair | `unpair PEER --yes --json` | `removed_node_id`, `name`, `remote_trust_removed` (false), `apply_required`, `apply_command` |
| Service | `service install [--verbose] --json`, `service enable/disable/start/stop/restart --json` | `action`, `service` |
| Service state | `service status --json` | Service object |
| Apply disk changes | `apply --json` | `applied`, `mechanism` (`user_service_restart`), `consent_may_be_required` |
| Pairing recovery inspection | `pair status --json` | `records` array: `remote_node_id`, `remote_name`, `transaction_id`, `state` |

`peer show PEER --json` aliases permission inspection; `peer remove PEER [--yes]` aliases unpair. Legacy `layout set EDGE PEER` and `layout remove EDGE` remain available. When a peer alias equals an edge name, `layout remove` prefers that peer; `remove-position` is unambiguous. Permission commands also accept `connect` and development-only `debug-input`. A control permission applies only to what that peer may do **on this computer**. Granting it never grants permission on the peer, authenticates a discovery entry or approves the portal. Denying it is enforced by the existing runtime permission checks; apply/restart is the predictable way to refresh all saved configuration.

A frontend must ask before unpair/removal, then explicitly add `--yes`. JSON commands never prompt on stdout. Human commands prompt on a terminal and reject noninteractive destructive edits without `--yes`. There is no pairing `--yes`. Unpair removes local trust, pairing recovery evidence and that peer's topology placement; removing an inner node by unpair also removes now-orphaned outer placements, retaining those other peers' trust. It cannot remove the other machine's trust. Changes are guarded by the existing edit lock and rechecked after human confirmation. Unpair saves the pruned config before removing trust; if the subsequent trust removal fails, report/re-query the partial local result and retry instead of claiming success.

## Payload semantics

`node`: configured `name` and public `node_id` (null before identity creation). Read-only commands do not create an identity. Runtime identity/name may differ after an unapplied edit; the snapshot retains the running values.

Each trusted peer has `name`, `node_id`, `trusted: true`, `permissions` (`connect`, `debug_input`, `control_this_node`, all booleans), `endpoint` (stored hint or null), `position` (`{x,y}` or null), and `availability`. Availability has `state` (`online`, `offline`, `unavailable`, `unknown`), `runtime_state` (raw sampled state or null), and `source` (`runtime_snapshot` or `none`). `online` means a fresh authenticated/ready/active observation, never permission by itself. Failed connection/authorization/backend preparation is `unavailable`, not proof the machine is powered off. No observation means unknown. Runtime phases are `Dormant`, `Connecting`, `Authenticated`, `Ready`, `Active`, `Unavailable`, `Stopped`; new phases must be treated as unknown.

`runtime` has `state` (`running`, `stopped`, `owned_unknown`, `unknown`, `unsupported`), `error` (string/null), `freshness_seconds: 5`, and `snapshot` (object/null). A snapshot contains `pid`, `updated` (Unix seconds), `name`, `node_id`, `state`, `focus`, `active_peer`, `active_coordinate`, `capture`, `receiver`, and `peers` (each `id`, `coordinate`, `availability`). Only fresh snapshots backed by current kernel lock ownership are reported. Backend state strings are diagnostics; obtain portal capability booleans from doctor. Snapshots are read-only advisory state, not a command channel or proof of instantaneous focus.

`service` has `available` (user-manager query succeeded), `load_state`, `active_state`, `sub_state`, `unit_file_state`, `main_pid` (number/null), and `error`. Missing properties are null. A loaded/enabled/active service does not prove peer or portal readiness. `health` reports `configuration_valid`, `identity_initialized`, `runtime_reporting`, `service_active`; combine these with layout issues, peer availability and doctor results. `configuration` exposes `loaded_at: runtime_start`, `apply_command`, and `disk_matches_runtime: null`: this release does not claim a live configuration comparison.

Each topology node has `local`, `name` (nullable for missing trust), `node_id`, `x`, `y`, `trusted`, `endpoint`. Local is `(0,0)`. Remote nodes remain cardinal coordinates at distance 1–2, at most eight placements. An outer node requires its inner neighbor. Duplicate coordinates, duplicate peer placements, local/diagonal/out-of-range placement and untrusted peers are rejected. Occupied coordinates are never silently overwritten. Move validates the entire resulting topology; it does not silently move dependent nodes. Remove the outer placement first when moving/removing an inner node would orphan it. Read views expose missing trust through `valid: false` and `issues: [{code: untrusted_peer, node_id}]`; malformed/duplicate config returns an error envelope.

Discovery returns running **and** pairing nodes. Every machine has `name`, `advertised_node_id` (nullable, unverified), `service_instance`, `addresses` (socket address strings), `control_port`, `pairing_available`, `pairing_version`, `input_version` (`[major,minor]` or null), `untrusted_hint: true`, `matches_trusted_id`, `local_permissions` (object/null), `configured_position` (object/null), `availability: discovered_unverified`. A pin match is a local lookup against an untrusted claim. Never label it authenticated or save a discovered endpoint without existing authenticated validation. Pair only entries with `pairing_available: true`; the port in those addresses is the pairing endpoint. Runtime/control discovery does not open pairing automatically. Pairing uses a separate DNS-SD instance name from the persistent runtime so a user can enter pairing mode while the service stays open.

Doctor reports the current process's graphical session, not another seat or the running daemon's environment. Each portal interface has nullable `version`, `keyboard_advertised`, `pointer_advertised`, and `error`. `consent_requested` is false; `authorization` and `physical_input` are `not_tested`. Portal version/device advertisement does not prove consent or delivered input. A missing/failed portal is visible in the payload; probe timeout is an error envelope. The Hyprland partial external-edge barrier limitation remains listed and unchanged. The pinned XDPH PR #426 trial remains separate development tooling, never installed/restarted by setup/apply/doctor.

## Structured pairing conversation

Use `pair --json [--interface NAME]` to listen temporarily, or `pair HOST[:PORT] --json [--expected-node-id FULL_ID]` to initiate. `--bind ADDR:PORT` supports explicitly scoped test listeners. To select visually, use discover JSON then pass a selected pairing address and its advertised ID through `--expected-node-id`; the actual cryptographic identity must match before confirmation. `pair --discover --json` is deliberately rejected because selection belongs to the frontend. The verified handshake/code ceremony is the existing Pairing Protocol 2.

Pairing is the only streaming command. Read newline-delimited JSON from stdout as it arrives and keep stdin open. Events use `schema_version: 1`, `command: pair`, `event`, `data`:

- `connecting`: `endpoint`, `timeout_seconds`.
- `listening`: `endpoint`, `node_id`, `name`, `discovery_available`, `timeout_seconds`.
- `discovery_unavailable`: `message`, `direct_pairing_available` (true).
- `confirmation_required`: `remote_name`, `remote_node_id`, `address_hint`, `verification_code`, `already_trusted_as` (string/null).
- `result`: the normal `ok`/`data`/`error` envelope plus `event: result`. Success data has `peer` (existing trust-record fields `id`, `alias`, `address`, `permissions`), `created`, `apply_command`; failure code is `pairing_failed`.

Display the full verified Node ID, remote name and verification code. Ask the user to compare codes on **both** machines. After their explicit choice, write one JSON line of at most 4096 bytes to stdin:

```json
{"accept":true,"remote_node_id":"FULL_VERIFIED_NODE_ID","verification_code":"EXACT_DISPLAYED_CODE"}
```

Use `accept: false` for rejection. Both identity and code must match this current ceremony; malformed/stale/mismatched replies fail closed. Closing stdin before answering rejects. SIGINT/SIGTERM cancels the command and removes temporary advertisement/listener; the whole ceremony expires after 90 seconds. Do not persist or log the verification conversation. A final result is required before reporting success; both peers must approve. Input permissions default to deny on newly created trust; existing trust keeps its settings. Interruption around commit may leave independently committed trust: inspect `pair status --json`, re-query peers, and offer the existing explicit terminal recovery workflow instead of silently deleting/replacing identity. Recovery states are `prepared`, `committed_unconfirmed`, `complete`, `conflict`, `expired`. Recovery is not an automatic pairing bypass.

## Applying changes and acceptance

`apply` validates config/trust and restarts only the installed user service through the existing current-session start path. It refuses a foreground/unknown owner rather than launching a second runtime. A fresh runtime snapshot must match the manager's MainPID before an owned runtime is treated as service-managed. The permanent-inode ownership lock remains the final exclusion guard. No signal reload, polling config writer, portal restart workaround or shell-profile hook is introduced. Restart closes active control and normal consent may be requested again. `applied` records successful restart submission/completion from systemd, not proof of portal consent; re-query health afterward. Enable/disable affect future login; start/stop affect the current service. Service commands are explicit user actions, never a consequence of read-only widget refresh.

Automated tests use isolated profiles, private pairing TCP endpoints/buses and systemctl doubles. They exercise structured reads/errors, missing trust, topology failures without writes, code-bound approval/rejection, permission scope, unpair confirmation, service errors, apply/ownership refusal, and local-only setup in a pseudo-terminal. The existing M15 runtime/login/trial regressions remain in the full suite. Physical M16 acceptance is still required on spectrum/io: inspect JSON, run setup while retaining existing trust/placement, make and revert a layout edit with apply, confirm normal control/ReturnControl, and verify logout/login. Do not create a new identity or unpair the proven machines solely to test destructive UI actions; use separate profiles for new pairing/unpair tests. M17 is unstarted.

## M18 local setup and backend readiness

`setup --local --json` is an explicit local write operation, suitable for a first-run button. It validates preferences before creating an identity, preserves existing identity, trust, permission and topology, and is idempotent. Optional `--name` validates 1–63 printable characters and renames only this computer. It never pairs, grants permission, requests portal consent, installs/enables/starts a service or restarts a running instance. A failure after identity creation can leave that identity initialized; re-query status and retry. Serialize this operation with other mutations. Normal `setup` remains the interactive terminal guide. `--yes` is not accepted for either setup form.

`doctor --json` adds `backend_state`: `backend_missing` (portal/interface absent), `incompatible_backend` (interface version/device capabilities insufficient), or `backend_ready` (InputCapture v1+, RemoteDesktop v2+, keyboard and pointer advertised). This is capability readiness, **not** a consent grant or proof of delivered input. `authorization: not_tested` remains authoritative. Frontends should show service failures separately and explain that starting/restarting may require approving the desktop's portal dialog. Do not classify permission denial by parsing diagnostic prose.

The optional `ukvm-portal-session` executable is a distro-integration helper for managed Hyprland portal units; it shares session validation with the development trial. It is not a widget API, input runtime, permission broker or general portal-restart command. See [LINUX_PRODUCTION.md](LINUX_PRODUCTION.md).

## M18 persistent desktop authorization

`status --json` and `doctor --json` add `desktop_authorization` with `remote_desktop` and `input_capture` entries. Safe fields are `state` (`persistent`, `active`, `needs_approval`, `unsupported`, `unavailable`), `active`, `remembered`, nullable `persistence_api_supported`, safe `reason`, and optional `error`. A remembered credential is not proof of restoration or delivered input. The portal remains the authorization authority; peer permissions remain independent. The legacy doctor authorization/physical-input evidence fields remain `not_tested`.

`authorization status --json` returns the two safe entries directly without opening a portal session. Unknown API support is null until an actual session or doctor version probe is available. `authorization reset --yes --json` requires explicit frontend confirmation. It closes the managed user runtime and forgets both remembered portal credentials; success returns `reset`, `service_stopped`, `service_restarted: false`, `trust_changed: false`, `control_permissions_changed: false`, and `desktop_authorization`. The user explicitly starts UKVM again to request normal portal approval. It refuses foreground/unknown ownership or another in-use diagnostic portal session and never modifies trust/topology. A failure after stopping must be treated as a possible stopped service, not success: re-query status and report the error. No token field or token-management file path is part of this contract.

RemoteDesktop v2 requests persistence, and InputCapture v2 uses its corresponding mechanism. Version 1 capture retains existing behavior and reports persistence unavailable. A v2 backend returning no token grants only the current session; it may have declined persistence or the user may not have selected remembering approval. Display **Active** rather than promising unattended restoration. See [portal persistence](PORTAL_PERSISTENCE.md) for security, backend identity, failure behavior and acceptance.

Authorization state additionally includes `context_verified`: the running portal frontend desktop context was observed independently of the caller environment. False means read-only status could not check that context; remembered state still denotes saved credentials, not proof of restoration. Runtime restores only with a verified portal context and normal portal/backend compatibility checks.

## M19 clipboard extension (additive schema 1)

`permissions.share_clipboard` is a boolean, default false when absent. `connect`
and clipboard permission must be allowed on both computers. This is bilateral
text sharing, independent of control permission, topology and desktop input consent.
Use `peer allow PEER share-clipboard --json` and `peer deny PEER share-clipboard --json`.
Success returns the existing permission payload plus `apply_required:false` and
`apply_command:null`; it applies live. Existing input/layout operations retain their
normal Apply semantics. Re-query status after every change. Pairing never grants it.

`status.data.clipboard` exposes only `supported` (boolean or null when unprobed),
`enabled` (any locally authorized sharing peer), `connected_peers` (count),
`last_sync` (Unix seconds or null), `state`, `error` (safe text/null),
`protocol_version:1`, `max_text_bytes:65536`. States are `disabled`, `stopped`,
`starting`, `ready`, `sharing`, `unavailable`, `restart_required` (older runtime still running). Runtime snapshots contain the same
safe sampled state; unknown old-runtime fields are not evidence of support.
`doctor.data.clipboard` adds `supported`, `backend:wayland_data_control`,
`protocol_version`, `max_text_bytes`, `contents_read:false`. Its probe never creates
a clipboard data device or requests text. Clipboard health is independent of portal
input health. Peer/discovery permission objects include the new boolean; no contents,
hashes, epochs or transfer frames are included in any frontend response.

Frontends should explain sensitive text sharing, confirm grants explicitly, display
its separate per-peer switch, and never request/parse clipboard contents. Ignore
unknown/additional schema fields and disable clipboard actions on older UKVM versions.
See [CLIPBOARD.md](CLIPBOARD.md) for protocol, lifecycle and physical acceptance.

## M20 file transfer (additive schema v1)

Peer `permissions.receive_files` is a boolean, default false, independent of input and clipboard. `peer allow|deny PEER receive-files --json` returns the authoritative permissions and `apply_required:false`; changes take effect immediately. A known running pre-M20 runtime must be restarted before enabling file receiving.

`status --json` adds `data.files`; `files status --json` returns the same object: `supported`, `protocol_version:1`, `state` (`available` or `restart_required`; unsupported on unimplemented OS), `receive_directory`, `max_chunk_bytes`, `max_entries`, `bytes_per_second`, and `transfers`. Receive availability additionally requires the normal runtime/service health. Transfers contain `id`, `peer_id`, `peer_name`, `direction` (`send`/`receive`), `name`, `state`, `bytes`, `total_bytes`, `entries`, Unix-second `started`/`updated`, nullable final basename `destination`, and nullable safe `error`. States: `preparing`, `connecting`, `transferring`, `verifying`, `completed`, `failed`, `cancelled`, `interrupted`. History is bounded to 64. No contents/hashes appear in this API.

`send PATH... PEER --json` accepts 1–64 literal local paths (no shell needed). It emits JSONL with schema_version 1, command `send`, event `started` or `progress`, data containing the transfer record; then exactly one event `result` with normal `ok/data/error` envelope. Success data: `id`, receiver-selected basename `destination`, `bytes`. Nonzero exit is failure; incomplete result means unconfirmed receipt. Frontends must inspect receiver history before retrying an uncertain finalization. Pairing's separate code-bound approval protocol is unchanged.

`files cancel ID --json` accepts only a known active 32-character lowercase hex ID and returns `cancellation_requested:true`, not a guarantee that a concurrently finalizing transfer was undone. `files directory ABSOLUTE_PATH --json` saves a receive root and returns `apply_required:true`; use the existing Apply flow. `files directory --json` reports the current configured/default receive root. Frontends must not modify config/history/permission files directly. Picker file URLs must be decoded to local paths by the frontend bridge; remote URL schemes are not transfer inputs. A future drop-on-peer action can invoke exactly this same send API.

See [FILE_TRANSFER.md](FILE_TRANSFER.md) for storage, limits, lifecycle and physical acceptance.

### Fresh controlled-node preparation

After granting `control-this-node`, the existing `apply_required:true` / `apply_command:"ukvm apply"` response remains unchanged. Apply retains `mechanism:"user_service_restart"`: general preference reload still requires the single managed lifecycle action. The restarted runtime proactively prepares its shared receiver if a pinned peer has both connection and control permission, so a fresh portal prompt no longer depends on an incoming control request. Preparation does not grant portal consent or enter control. Frontends should query the existing runtime receiver and desktop authorization fields afterward; `applied:true` never promises that the user approved a pending dialog. Revocation uses live peer policy checks and Apply closes prior receiver state; with no eligible peer, startup does not request consent. No JSON/JSONL fields or protocol versions change.

## Interactive Remote Sessions v1 (additive schema v1)

Permission objects add `view_screen` (default false when absent). Receiver-side
`peer allow|deny PEER view-screen --json` applies live on the new runtime and
returns `apply_required:false`, `apply_command:null`; a running older runtime must
be restarted first. Trust, viewing, control, clipboard and files remain separate.
Runtime snapshots add nullable `display_protocol_version` (1 in this release).
`ukvm remote PEER` launches a native Wayland window and is deliberately a window
command rather than a JSON stream. The existing remote-input permission and Apply
semantics stay unchanged. No frame bytes, pixels or key payloads enter JSON.
See [REMOTE_SESSIONS.md](REMOTE_SESSIONS.md) for behavior and physical tests.
