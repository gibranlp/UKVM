# Linux management CLI contract — schema version 1

M16 adds Linux configuration and management UX over the existing M15 runtime. A frontend invokes the installed `ukvm` executable as the logged-in user with an argument array, inherits that user's graphical/XDG environment, and parses JSON. It never edits config, certificates, trust, runtime snapshots or systemd files itself. UKVM has no SpectrumOS dependency. No widget, macOS/Windows work, input-protocol change or M17 feature is included.

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
