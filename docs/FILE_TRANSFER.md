# Secure file transfer (M20)

M20 adds Linux-to-Linux files independently of input and plain-text clipboard. M18 control/persistent authorization and M19 clipboard are user-confirmed physically accepted. M20 is automated-tested; Spectrum ↔ Hellgen physical acceptance is pending. Windows/macOS filesystem backends are not implemented.

## Permission and daily use

On the receiving computer explicitly authorize each trusted sender:

```sh
ukvm peer allow SENDER receive-files
ukvm peer deny SENDER receive-files
```

The default is deny. Receiving requires both pinned peer trust/connection permission and `receive-files`; neither control nor clipboard grants file access. Revocation applies while connected without restarting UKVM. A new M20 executable refuses enabling this permission while a known older runtime is running: restart the upgraded user service first. A false permission is omitted from stored TOML to retain older-reader compatibility until explicitly enabled. JSON always includes the boolean.

```sh
ukvm send photo.jpg Hellgen
ukvm send document.pdf photo.jpg Hellgen
ukvm send ./folder Hellgen
ukvm files status --json
ukvm files cancel TRANSFER_ID --json
ukvm files directory /absolute/receive/directory --json
ukvm apply
```

Directory changes use the existing apply/service path. Permissions do not require Apply. Receivers need the normal user runtime listening; senders invoke the installed CLI and need no SSH, mounted share, shell service or independently running local runtime. Endpoint hints are verified against pinned TLS identity. A missing endpoint can be discovered through mDNS; discovery carries no file data or trust.

The Linux default is the XDG Downloads directory plus `UKVM`, falling back to `~/Downloads/UKVM`. Configurable receive roots must belong to the current user and must not allow group/other writes. Symlinks anywhere in source/destination path resolution are rejected. Missing directories are created privately. Received files are mode 0600 and directories mode 0700; no ownership, executable bits, device metadata or permissions are copied.

## Protocol and responsiveness

File Protocol **1** uses ALPN **`ukvm-files/1`** over the existing mutually authenticated/encrypted TLS listener. Each explicit transfer gets its own connection, independently of Input Protocol 1.3 and Clipboard Protocol 1. Old peers can continue input/clipboard; an unsupported file capability produces a file-only failure. There is no relay and no transfer API for arbitrary commands.

The fixed-integer bincode wire format is length-prefixed (big-endian u32 frame length). A bounded Hello negotiates version 1 and 65,536-byte chunks. Offer supplies a random 128-bit transfer identifier, ordered directory/file entries, UTF-8 component arrays and checked sizes. The receiver accepts/rejects before any payload. Every chunk has file index and exact offset and requires a bounded cumulative acknowledgment. FileEnd supplies exact size and SHA-256; directories contain no payload. Complete requests atomic publication, Result reports the final basename, and Cancel/Rejected terminate safely. Future MIME/content/version capabilities require explicit negotiation; they cannot be injected into this version's metadata.

Limits: 64 selected roots; 10,000 entries; 64 path components; 240 UTF-8 bytes per component; 4,096 component bytes per path; 1 TiB per batch; 4 MiB maximum frame. Files are streamed in 64 KiB buffers, never buffered wholesale. Blocking filesystem and progress/history work runs off the input event executor. Separate queues, per-chunk acknowledgment and a cooperative **16 MiB/s cap per transfer** preserve headroom; there are at most four transfers per direction per user. This cap does not replace network QoS, and interactive responsiveness on slow/saturated physical links still needs acceptance testing.

## Filesystem boundary and integrity

All received components are hostile. Absolute paths, traversal, separators, controls, reserved Windows device names, nonportable characters, case-insensitive duplicate entries, missing parents and invalid sizes are rejected. Source inventories reject symlinks, device nodes, sockets and FIFOs rather than silently skipping them. Non-UTF-8 filenames are unsupported. Sender-side source modification fails if its declared size changes; transmitted bytes always receive end-to-end digest verification.

Linux uses directory descriptors and no-follow relative opens, exclusive file creation and no-replace atomic rename. Receive-root ownership/write permissions prevent another user from replacing staging entries. SHA-256, byte counts and every file finalization must pass before the entire batch becomes visible. Free space and free inode checks precede acceptance (with 64 MiB/16-entry reserves); write failures still fail closed. Concurrent transfers can exhaust remaining space after the initial check; there is no quota/reservation feature yet.

A hidden private `.ukvm-incomplete-*` directory holds the batch. A single file/folder publishes under its original name. Multiple selected roots publish together under `UKVM files TRANSFER_ID`. Existing entries are never overwritten: `report.pdf`, `report (1).pdf`, etc. Linux `RENAME_NOREPLACE` resolves concurrent collisions atomically. SHA-256 is computed by the sender and verified by the receiver before finalization; TLS independently authenticates the sender and protects transport.

Normal cancellation, EOF, permission loss, SIGTERM/service shutdown or verification errors remove staging. Abrupt SIGKILL/crash can leave hidden incomplete directories; they are never reported as completed and are not automatically trusted or resumed. Automatic crash-orphan garbage collection and resumable transfer are future work. Once verified publication has occurred, cancellation cannot undo it. A lost final acknowledgment can mean the receiver completed while the sender reports failure: inspect receiver history before retrying. Final filesystem sync failure likewise preserves already verified published data rather than overwriting/removing it.

## History/frontend

Private per-user state under `$XDG_STATE_HOME/ukvm/file-transfers` holds at most 64 transfer records (user-only, atomic replacement). Permanent-inode locks distinguish active owners from interrupted processes after restart. Explicit cancellation markers are accepted only for a known active identifier. History provides sender/peer, name, direction, state, bytes/total, timestamps, final basename and safe errors. Neither payload nor digest is logged or exposed in CLI status, JSON or the shell. Wire message Debug redacts payload and hashes.

`status --json` adds `data.files`; peers add `permissions.receive_files`. `files status --json` reports the same file state. `send --json` emits version-1 JSONL `started`, `progress`, then one `result` envelope. See FRONTEND_CONTRACT.md. The SpectrumOS widget uses this contract for Receive files, file/folder selection, progress and cancellation; QML implements no transport/storage logic. Closing the widget does not cancel its active subprocess. Explicit Cancel, shell process shutdown or receiver runtime shutdown terminates the transfer.

## Validation and physical acceptance

Automated tests cover streamed text/binary/empty files, Unicode/spaces, recursive empty directories, multiple selections, concurrent collisions, pinned TLS/version negotiation, default-deny and live revocation, hostile paths/symlinks/FIFOs, digest/size/order corruption, interrupted/cancelled transfers, bounded histories/private state, and concurrent TLS input heartbeats and clipboard messages during bulk transfer. Existing control/portal/clipboard tests remain required. Heartbeat/clipboard tests do not prove physical pointer responsiveness.

On both machines install the M20 packages and restart UKVM once. Use each machine's actual trusted peer alias if it differs from Spectrum/Hellgen.

```sh
# Spectrum
ukvm service restart
ukvm peer allow Hellgen receive-files
mkdir -p /tmp/ukvm-m20-test/folder
printf 'hello M20\n' > /tmp/ukvm-m20-test/sample.txt
printf 'Unicode: 世界\n' > '/tmp/ukvm-m20-test/folder/你好 file.txt'
sha256sum /tmp/ukvm-m20-test/sample.txt
ukvm send /tmp/ukvm-m20-test/sample.txt Hellgen
ukvm send /tmp/ukvm-m20-test/folder Hellgen
ukvm send /tmp/ukvm-m20-test/sample.txt Hellgen
```

```sh
# Hellgen (enable before Spectrum sends; use reported collision names if needed)
ukvm service restart
ukvm peer allow Spectrum receive-files
sha256sum ~/Downloads/UKVM/sample.txt
cmp ~/Downloads/UKVM/sample.txt ~/Downloads/UKVM/'sample (1).txt'
cat ~/Downloads/UKVM/folder/'你好 file.txt'
ukvm send ~/Downloads/UKVM/sample.txt Spectrum
ukvm peer deny Spectrum receive-files
```

On Spectrum verify the returned sample hash. Attempt the send again: it must fail; input and bidirectional clipboard must continue. On Hellgen re-enable `receive-files`. On Spectrum create a large test file and send it while physically controlling Hellgen, scrolling/typing/using clipboard and ReturnControl; verify its SHA-256 on Hellgen. Restart the receiver service, send again, then reboot the receiver and repeat. In the widget independently test permission reflection, Send file/Send folder, cancellation/progress and surfaced denial. No existing trust is removed.
