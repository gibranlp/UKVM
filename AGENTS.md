# UniversalKVM agent guide

Read this file before changing UKVM. Consult the relevant document under `docs/` for the area being changed. Keep documentation aligned with code.

## Product

UniversalKVM (`ukvm`) is a cross-platform software KVM. Each computer keeps its own monitor; moving across a configured screen edge eventually hands keyboard and pointer control to another computer. Every installation contains both controller and controlled capabilities. Role is selected through configuration and runtime state, not by installing separate server/client binaries. Targets: Linux Wayland, Linux X11, macOS and Windows. Wayland is a first-class target.

Terms: **node** = one installation/process; **controller** = node capturing local input for a peer; **controlled** = node injecting authenticated input locally; **peer** = another node; **discovery** = finding candidates, never granting trust; **pairing** = explicit trust authorization; **focus** = current input recipient; **session** = one authorized control relationship.

This is not remote desktop. Do not add screen streaming, remote shell, arbitrary commands or GUI work to the foundation. Future clipboard, file transfer, drag/drop, GUI/TUI, hotkeys, advanced display mapping and statistics need separate design and approval through normal project review. Do not portray planned features as implemented.

## Current workspace

- `crates/ukvm-cli`: `ukvm` executable and presentation only.
- `crates/ukvm-core`: platform/transport independent roles and focus concepts.
- `crates/ukvm-config`: TOML preferences and OS config path. Never put private keys or trust records in this config.
- `crates/ukvm-protocol`: versioned semantic events and provisional debug-session wire messages.
- `crates/ukvm-network`: mutual TLS transport, bounded framing, persistent identity, pinned peer trust records, authorization and DebugSink.
- `crates/ukvm-platform`: platform input/display traits and an experimental Linux Wayland portal/EI capture path. Generic trait adapter and injection are not implemented.
- `docs/ARCHITECTURE.md`, `PROTOCOL.md`, `SECURITY.md`, `PLATFORM_SUPPORT.md`, `ROADMAP.md`: design and limitations.

There are no `ukvm-discovery` or `ukvm-security` crates yet. Add a crate only when concrete code and a stable responsibility warrant it. Discovery and transport must remain separate. Configured peer trust lives in user state, separate from preferences and the node private key. Layout links use cryptographic peer IDs, while aliases and addresses are labels and connection hints. Peer recognition never grants input permission; DebugSink and remote-control permissions are separate and default to deny. The current listener is a development DebugSink, never an injector. Keep the CLI and future background runtime independent so the same process can support inbound and outbound control and later user-session service packaging.

## Security rules

Read `docs/SECURITY.md` before changing network, pairing, protocol, storage or injection code. Treat discovered peers and all network bytes as untrusted. The development DebugSink requires manual public-certificate exchange and mutual TLS; real input injection additionally requires future explicit pairing and authorization policy. Peer addresses and displayed names do not prove identity. Never invent cryptography, disable peer verification, log keystrokes/secrets, auto-trust discovered machines, or add remote command execution. Protect private keys using suitable OS/user storage; keep them outside config TOML. Bound message sizes, queue depths and rates; malformed packets must be rejected safely. Stop injection and release held keys/buttons on lost focus, permission loss or disconnect. Require a local override path.

## Platform rules

OS input capture, injection, pointer position, displays, boundaries and clipboard belong behind `ukvm-platform` traits and target-specific modules. No OS-specific calls in core, protocol, config, CLI policy or future transport. Report individual capabilities and permission failures at runtime. Do not assume one OS's input or coordinate model applies elsewhere. Map physical keys carefully; test modifier state, button releases, scaling and injected-event loops.

For Wayland, investigate XDG Desktop Portal InputCapture, RemoteDesktop and libei first. Respect compositor and user grants. If a compositor lacks an API, report the limitation. Never force XWayland, disable Wayland security, use privileged `/dev/input` access as a universal workaround, or imply portal support is universal. Do not silently fall back from a native Wayland session to X11. macOS and Windows likewise need sanctioned APIs and session/permission handling.

## Coding and dependencies

Use stable Rust, idiomatic ownership and explicit errors. Keep abstractions small and tied to real seams. Prefer safe Rust; isolate and explain any necessary FFI or `unsafe` in platform modules with invariants and tests. Do not add another implementation language without a clear platform-specific reason. Choose maintained, established crates for transport, cryptography and parsing; review security and licenses. Avoid custom cryptography, bespoke transport framing without a protocol review, unnecessary micro-crates and placeholder implementations that report false success. The current workspace supports Rust 1.87+ and edition 2021; update MSRV deliberately. Linux Wayland portal code uses `ashpd` and `zbus`; EI input uses `reis`. Consult `docs/PLATFORM_SUPPORT.md` for dependency rationale and test evidence.

## Testing and documentation

Run these from the repository root after relevant changes:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
```

Tests should cover meaningful policy, version compatibility, parsing, malformed input and backend capability behavior. Platform backend claims require tests on actual target OS/session and, for Wayland, representative compositors. Record untested platforms honestly. Keep `README.md` and `docs/` aligned with reality; update protocol versioning and security notes when behavior changes. Do not mark a CLI command or backend operational until it really works end to end.

## Two-computer testing and Git attribution

When a change needs to be checked on the other computer, complete local review and validation, then commit and push the relevant code to the configured Git remote before giving instructions to pull or test it there. Verify that the push succeeded and report the commit ID. If it cannot be pushed, say so clearly; do not present an unpublished change as ready for the other computer. Check the staged files for secrets and build output before committing.

Use the repository owner's configured Git identity. Do not add Codex as an author, co-author, contributor, or commit trailer, and do not add Codex to a contributors list.
