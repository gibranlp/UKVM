# Windows private state and ownership — M22

Implemented foundation, native acceptance pending. The test PC runs Windows 11
without Rust. Native capture/injection, startup installation and remote viewing
are not implemented. `run`, `control` and `listen --remote` still reject Windows
input use. Linux input and saved portal authorization remain unchanged.

## Private state

`ukvm-platform::windows::state` owns the Win32 filesystem/security calls used by
identity, pinned certificates, trust edits and pairing recovery. New objects
receive a protected DACL granting full access only to the current process user.
Existing owner/DACLs are checked without supplying creation security. Unsafe
state is rejected without repairing permissions, regenerating identity or granting
trust. DER keys use user access control; this does not isolate malicious code
already running as that user or an administrator.

Storage accepts ordinary absolute local paths on fixed NTFS/ReFS drives. It
rejects UNC/device/verbatim paths, traversal, ADS, reserved names, reparse points
and extra file hard links. Ancestor handles omit delete sharing and remain held
during operations. Object type, owner, protected DACL and variable ACE/SID extents
are checked through handles. Lock handles also omit delete sharing for their
lifetime. Read-only operations create no state. See [CreateFile](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
and [security queries](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo).

Reads are bounded to 4 KiB for keys/certificates, 1 MiB for trust and 64 KiB for
recovery; those bounds also apply on Unix, retaining its modes and write/sync
behavior. Identity Debug redacts private bytes. Windows replacement flushes and
closes a private temporary file, then uses same-directory MoveFileEx with
replace/write-through and no copy fallback. Security-state removal flushes an
in-place revocation marker before deletion by handle. A surviving revoked receipt
cannot be loaded/recreated by its old transaction. Advisory snapshots need no
revocation marker. See [MoveFileEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).
Native crash/reboot and durability acceptance remain required; no power-loss
proof is claimed from compiler or compatibility tests.

The network crate depends on platform primitives only on Windows; Linux transport
gains no production portal dependency. `windows-sys` 0.61.2 (MIT/Apache-2.0,
Rust 1.71 minimum) fits UKVM's unchanged Rust 1.87 MSRV. FFI stays in platform
modules, with owned versus borrowed handles, documented allocation cleanup,
aligned/bounded token buffers and checked variable SID extents. Errors contain
no credentials or payloads.

## Session and ownership

The read-only probe queries the current process session, elevation, window station,
thread desktop and input desktop. Acquisition requires a nonzero active session,
non-elevated token, WinSta0 and Default desktop. Failures and secure/disconnected
desktops fail closed. It never switches desktops or requests capture/injection.
Future input actors must recheck health throughout their lifecycle. See
[session queries](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsquerysessioninformationw)
and [OpenInputDesktop](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop).

Windows ownership uses a permanent private `runtime.lock` under
LocalAppData/ukvm/runtime, with a kernel lock per user across sessions. It never
unlinks the lock to unlock; process exit releases ownership. Read-only status
creates no directories/locks and ignores snapshots without an owner. Portable
snapshots preserve the existing Linux/clipboard schema, are bounded to 64 KiB
and expire after five seconds. Invalid/stale state reports unknown. Ownership is
not yet wired into a Windows input runtime or service launcher.

`doctor --json` reports safe Windows session metadata and probe errors, with
backend `unsupported`, input implementation false, physical input `not_tested`
and storage validation `not_requested`. Session readiness never grants peer trust.

## Windows 11 test kit

The kit contains the executable, platform/runtime/network tests, command runners,
checksums and dependency notices. It needs no Rust. Unpack in a local folder and
run `Check-Foundations.cmd` from a normal, non-elevated terminal. It queries doctor
and tests isolated temporary fixtures; network tests use loopback TLS and mock
sinks. It installs no service, edits no existing identity/trust, contacts no real
peers, accesses no clipboard, captures/injects no input and resets no consent.

Report every failure and keep the output. ACL-change and directory-lock checks
remain strict. `Check-Reparse.cmd` runs the ignored reparse fixture with existing
symlink privilege/Developer Mode; unavailable privilege is not a pass. Native
execution, process-crash recovery, standard-user access and second-user ACL denial
are acceptance gates before Windows input is enabled.

CI schedules the full Windows workspace suite; the symlink fixture is explicitly
ignored pending its native privilege. Linux checks and Windows cross-compilation
are separate from native execution. Wine adds compatibility coverage and cannot
prove native ACL/directory-sharing enforcement. Native results remain pending.

Local evidence: all five Linux workspace checks passed (312 tests, six opt-in
tests ignored). Windows GNU whole-workspace Clippy, test compilation/linking and
build passed. Isolated Wine execution passed 62 network/state tests and five
ownership tests, including abrupt child termination. Its directory-sharing
rename assertion fails; that assertion is retained for native Windows acceptance,
alongside the privilege-dependent reparse fixture. No native security acceptance
is inferred from these compatibility results.
