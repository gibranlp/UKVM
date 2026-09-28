# First-contact pairing (protocol 2)

Pairing bootstraps the existing pinned-certificate trust store. It does not grant input permission or choose a layout edge. The UKVM input protocol remains version 1.1 and continues to use mutual TLS with pinned certificates.

## Threat model and choice

An unknown LAN host may observe, alter, replay, initiate or drop pairing traffic. A displayed name or IP address is untrusted. A malicious host may try to occupy the one pairing slot, guess the verification value, substitute a certificate, or cause one side to lose its connection during trust storage. A compromised local terminal or a user who confirms mismatched codes is outside the protection of this ceremony. Pairing does not repair a compromised long-term private key.

Candidate designs considered:

| Design | Fit |
| --- | --- |
| SPAKE2/other PAKE | Appropriate when one node privately conveys a password to the other. A displayed code compared on both nodes would require a different UX; sending the password over the network would defeat the point. |
| Noise with short authentication string (chosen) | Fits two terminals displaying and comparing the same value. Noise handles fresh ephemeral key exchange and encrypted messages; the human comparison authenticates the otherwise unknown peers. |
| Bare ephemeral Diffie–Hellman plus SAS | Could work, but would require more bespoke channel construction and state management. |

UKVM uses [`snow` 0.9.6](https://github.com/mcginty/snow) to implement the specified [`Noise_NN_25519_ChaChaPoly_BLAKE2s`](https://noiseprotocol.org/noise.html) handshake and transport. The long-term UKVM TLS private key is **not** used as a Noise key and never goes on the wire. Both sides send their existing public DER certificate, its claimed SHA-256 node ID, and configured node name inside the encrypted Noise transport. Each side validates the certificate syntax and recalculates its node ID. Each then signs a domain-separated digest of the Noise handshake hash and both certificates/names with its existing ECDSA P-256 identity private key. The other side verifies that proof with the public key parsed from the certificate using [`x509-parser`](https://github.com/rusticata/x509-parser) and [`ring`](https://github.com/briansmith/ring). This proves possession of the identity key before the confirmation prompt. Existing UKVM-generated identities use P-256; another key algorithm is rejected until explicitly supported. The Noise prologue includes the pairing protocol version. The encrypted messages use bounded, version-specific `bincode` frames; they cannot decode as input protocol messages.

The displayed eight-digit `####-####` short authentication string (SAS) is the first eight bytes of a domain-separated SHA-256 digest, reduced modulo 100,000,000. The digest includes length-prefixed Noise handshake hash, initiator certificate, responder certificate, initiator name and responder name in that order. The code is a comparison value, **not** a password sent over the network. A successful comparison binds the two public identities and names to this fresh exchange. Its roughly 27 bits limit active impersonation resistance to a human-verified, single-attempt ceremony; repeat attempts demand renewed local pairing mode. The code is not stored.

## Transport and lifecycle

Pairing uses a separate temporary TCP listener on port **24801**. The ordinary mTLS/input listener stays on port 24800. A length-bounded `UKVM-PAIR` magic/version packet precedes the Noise exchange; incompatible versions fail before identity exchange. Protocol 2 adds an encrypted `New`/`Recover(transaction)` mode discriminator and durable commit recovery. Protocol 1 peers need the newer build on both computers; the input protocol stays at 1.1. The listener accepts one connection and the entire command expires after 90 seconds or Ctrl+C. One active attempt and a 90-second lifetime limit trivial request spam; the first connection can still occupy the slot until timeout. There is no discovery, permanent pairing daemon, privileged port or firewall modification.

Sequence:

1. Initiator connects; both exchange pairing magic/version and run fresh Noise NN.
2. Both exchange encrypted public identities, validate certificate ↔ node ID, and verify a signature proving the peer holds its corresponding long-term key.
3. Both display the same SAS and remote name/ID. **Both users compare the codes on their own screens and explicitly answer yes.** A rejection or disconnect creates no trust.
4. After exchanging affirmative decisions, each node writes a durable `Prepared` recovery record and rechecks its trust store. Both exchange encrypted readiness results. No trust is written unless both report ready.
5. Each writes an ordinary Milestone 8 peer record under the existing edit lock, advances its recovery state to `CommittedLocally`, and sends an encrypted commit result. A received positive result advances the local state to `Complete`. A lost result leaves an inspectable uncertain state and **does not undo** a durable commit.

The trust record defaults to `connect=true`, `debug_input=false`, `control_this_node=false`. An existing certificate is reported under its current alias and keeps its address and permissions. A reused alias with a different certificate is rejected; key rotation is a separate future procedure. Address hints come from the authenticated pairing socket's peer IP and use the ordinary UKVM port. Pairing does not set a layout edge.

Milestone 9 was physically proven between `spectrum` (`192.168.1.21`) and `io` (`192.168.1.40`): the real computers connected, displayed matching codes, received explicit confirmation on both terminals, stored each other's exact cryptographic identities with safe permission defaults, and needed no `.der` exchange. The earlier isolated localhost test also used freshly paired identities for an authenticated DebugSink event after explicitly granting DebugSink permission. Protocol 2 recovery is covered by local automated tests; a physical protocol 2 regression and interruption test still need the other computer.

## Commit audit and recovery

Protocol 1 exchanged confirmation, checked readiness, wrote trust on each computer, sent `Commit(true)`, and removed newly written trust if the final exchange failed. The failure window was asymmetric: A could receive B's commit and report success while B lost A's acknowledgement and removed A. A process crash after trust storage could likewise bypass the rollback. Network loss, Ctrl+C, shutdown, and filesystem errors at the readiness/write/final-ack boundaries had no durable transaction evidence.

No two-computer protocol can guarantee that both sides simultaneously *know* the other durably committed when either can fail at any instant. Protocol 2 makes the result explicit and recoverable:

| Durable state | Local trust | Meaning |
| --- | --- | --- |
| No record | absent | Rejected or failed before mutual confirmation; start again. |
| `Prepared` | absent | Both users confirmed, but no local trust was written. Remote may or may not have committed. |
| `Prepared` | exact peer present | Crash after trust write but before state update; treat as uncertain. |
| `CommittedLocally` | exact peer present | Local trust is durable; remote acknowledgement is missing. |
| `Complete` | exact peer present | Both commits were observed; receipt is retained temporarily for the other side's recovery. |
| Any state | different certificate/alias conflict | Conflict; fail closed and inspect manually. |
| Expired | any | Recovery is disabled; existing trust is not removed. |

Each new ceremony derives a 64-hex-character, nonsecret transaction ID from a domain-separated SHA-256 digest of the *fresh* Noise handshake hash and both ordered certificates. The ID cannot substitute for the certificate or the SAS. A recovery record under `$XDG_STATE_HOME/ukvm/pairing/<transaction-id>.toml` (Linux default `~/.local/state/ukvm/pairing/`) contains the ID, local and remote node IDs, remote public DER certificate, remote name and address hint, a mutual-confirmation marker, creation time, and commit state. It contains **no** Noise ephemeral key, verification code, long-term private key, or input permission. The directory is mode 0700 and records mode 0600 on Unix; writes use the existing edit lock, a synced temporary file, atomic rename, and directory sync. The original trust store remains authoritative for actual peer permissions.

`ukvm pair status` reads these records and the current trust store. `ukvm pair recover` on one computer listens temporarily; `ukvm pair recover HOST [TRANSACTION_ID]` on the other initiates. A fresh Noise connection and fresh P-256 identity proofs authenticate both peers. Recovery requires **matching, unexpired, mutually confirmed pending records on both sides**, the exact original certificate and node IDs, and the same transaction ID. Merely claiming a transaction or claiming to trust the other node never grants trust. If both already have exact reciprocal records, recovery marks completion without rewriting alias, address, layout, or permissions. If one side has exact trust and the other still has `Prepared`, only the missing side imports the originally confirmed certificate, with normal default-deny input permissions. If neither committed, both report that a new human-confirmed ceremony is required. A mismatch, missing record, alias conflict, or expired record fails closed. Recovery can be repeated after another disconnect.

Records are retained for **seven days**, separate from the 90-second active ceremony timeout. `ukvm pair cleanup` removes expired recovery metadata without removing trust. An explicit `ukvm peer remove` invalidates that peer's retained recovery records so recovery cannot restore intentionally revoked trust. A manually edited or corrupt trust store causes recovery to fail. A changed source IP is only a connection hint; identity is the exact certificate. A new pairing ceremony remains available when recovery is impossible.

## CLI and safe physical trial

`ukvm pair` listens temporarily; `ukvm pair HOST` initiates. An explicit `HOST:PORT` or `ukvm pair --bind ADDR:PORT` supports isolated localhost tests. Both commands use the existing persistent identity unless pointed at isolated XDG directories. They never regenerate an identity whose files already exist. Normal successful pairing does not show transaction internals; interrupted pairing tells users to run `ukvm pair status`.

The known-good configuration should remain untouched during a protocol 2 physical regression. On **each** PC, start a fresh shell and set three temporary XDG directories before running any UKVM command:

```sh
UKVM_PAIR_PROFILE="$(mktemp -d)"
export XDG_CONFIG_HOME="$UKVM_PAIR_PROFILE/config"
export XDG_STATE_HOME="$UKVM_PAIR_PROFILE/state"
export XDG_DATA_HOME="$UKVM_PAIR_PROFILE/data"
mkdir -p "$XDG_CONFIG_HOME/ukvm"
```

On B, set its temporary node name and wait:

```sh
printf '[node]\nname = "io"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
ukvm identity
ukvm pair
```

On A, in its own temporary shell:

```sh
printf '[node]\nname = "spectrum"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
ukvm identity
ukvm pair 192.168.1.40
```

Compare the eight-digit value physically on both screens before answering yes on **both** terminals. Then on each machine run `ukvm peer list`, `ukvm peer show <remote-alias>`, and `ukvm peer permissions <remote-alias>`. The remote ID must match that shell's peer certificate fingerprint, and both input permissions must remain denied. Exiting these shells returns to the original XDG paths and known-good trust records. Keep the temporary directories until results are recorded; they contain newly generated private identities.

For an interruption test, use *separate* temporary profiles and a deterministic test harness or carefully planned termination after confirmation. Do not attempt to time a millisecond-scale final acknowledgement by hand. Run `ukvm pair status` on both nodes, then `ukvm pair recover` on B and `ukvm pair recover 192.168.1.40 TRANSACTION_ID` on A if both show the same recoverable transaction. Verify reciprocal IDs and permission defaults afterward. A physical interrupted-commit test has not yet been run.
