# First-contact pairing (protocol 1)

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

Pairing uses a separate temporary TCP listener on port **24801**. The ordinary mTLS/input listener stays on port 24800. A length-bounded `UKVM-PAIR` magic/version packet precedes the Noise exchange; incompatible versions fail before identity exchange. The listener accepts one connection and the entire command expires after 90 seconds or Ctrl+C. One active attempt and a 90-second lifetime limit trivial request spam; the first connection can still occupy the slot until timeout. There is no discovery, permanent pairing daemon, privileged port or firewall modification.

Sequence:

1. Initiator connects; both exchange pairing magic/version and run fresh Noise NN.
2. Both exchange encrypted public identities, validate certificate ↔ node ID, and verify a signature proving the peer holds its corresponding long-term key.
3. Both display the same SAS and remote name/ID. **Both users compare the codes on their own screens and explicitly answer yes.** A rejection or disconnect creates no trust.
4. Both send an encrypted readiness result, reload and check the local trust store, then acquire the same edit lock used by peer CLI commands.
5. Each writes an ordinary Milestone 8 peer record and sends an encrypted commit result. If either result fails or the connection ends before acknowledgement, newly written trust is removed. Existing trust is preserved.

The trust record defaults to `connect=true`, `debug_input=false`, `control_this_node=false`. An existing certificate is reported under its current alias and keeps its address and permissions. A reused alias with a different certificate is rejected; key rotation is a separate future procedure. Address hints come from the authenticated pairing socket's peer IP and use the ordinary UKVM port. Pairing does not set a layout edge.

In an isolated localhost CLI trial, temporary nodes named `spectrum` and `io` displayed the same eight-digit value, both committed pinned records, and `ukvm peer list` showed the opposite certificate ID on each side. Automated tests then enabled DebugSink permission explicitly and used those paired identities for an ordinary authenticated DebugSink event. A physical PC A/PC B pairing test is still pending.

There is no perfectly atomic commit across two computers under arbitrary network failure. A disconnect at the final acknowledgement boundary can leave an uncertain one-sided record; UKVM reports failure on the side that did not receive acknowledgement and attempts rollback. Inspect `ukvm peer show` on both nodes before relying on an interrupted ceremony. Even a leftover record grants no DebugSink or remote-control input permission. Unexpected process termination after local persistence but before rollback is another narrow uncertainty; automatic recovery/journaling is future work.

## CLI and safe physical trial

`ukvm pair` listens temporarily; `ukvm pair HOST` initiates. An explicit `HOST:PORT` or `ukvm pair --bind ADDR:PORT` supports isolated localhost tests. Both commands use the existing persistent identity unless pointed at isolated XDG directories. They never regenerate an identity whose files already exist.

The known-good Milestone 8 configuration should remain untouched during the first physical pairing test. On **each** PC, start a fresh shell and set three temporary XDG directories before running any UKVM command:

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
