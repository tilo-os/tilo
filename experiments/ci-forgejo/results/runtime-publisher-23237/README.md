# Current runtime publisher identity

The current protected `RUNTIME_BUNDLE_SIGNING_KEY` resolves to public Ed25519 key:

`50d099efd7805a4783847f11ba2dd4ecf338e2f6b254a3f8eb5822eec21f3dd1`

This is fresh independent evidence from actual repository `qos/qos`,
[run 9734](https://code.podesta.ai/qos/qos/actions/runs/9734) (API **23237**),
job **85267**, attempt **1**, task **76103**.
The single manual dispatch named exact reviewed source
`c7a83a4f06614d881676cad6a37056572e3b5930`; the job also required that source to
match its explicit expected-source input. It ran from `2026-09-19T04:37:18Z`
to `2026-09-19T04:37:23Z`. There was no retry or alternate credential.

The hash-bound approved native bundle
`10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde`
(6,113,401 bytes) supplied runtime CLI
`a5acd17e85ad7715d530b6ef10a8fefa1a7f875421dea8dcaffebcfbeaf77838`.
The job verified both digests before executing the CLI inside its exact compatible
builder image. Package and image reads used the dedicated read:package credential.
No download or installation of third-party helpers occurred.

The CLI's `key pubkey` command reads the identity through `/proc/self/fd/0`, with
private JSON supplied only on the child process's stdin and no inherited secret
environment. This executes the production `Identity::load` / `from_json_bytes` /
`public_key_hex` path: a JSON `secret_key_hex` value is a 32-byte Ed25519 seed.
The relevant `lib.rs` and `main.rs` Git blobs are identical between the approved
canonical-tool source and current merged production main; adjacent JSON records
retain those source identities.

The workflow exposes this signing secret to one guarded step only. It disables
core dumps, writes only the verified executable to a temporary directory, captures
child stderr without printing it, and logs only a strictly validated public-key
result plus public source/tool hashes. No private JSON, seed, error body, or
credential was stored in a file, command argument, log, output artifact, or proof.

The public key matches a historical fixture, but this proof establishes it from
the current secret independently. No key was generated or rotated. No app build,
publication, trust-policy edit, activation variable, or full-collection dispatch
occurred. The next policy preparation can bind this runtime publisher alongside
the separately established application publisher.
