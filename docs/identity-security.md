# Identity signing boundary

Tilo applications do not receive private identity material. For each authored
launch, `tilo-launcher` resolves the selected identity on the host, opens exactly
one Unix socket pair, retains the signing share, and places the app end at file
descriptor 15. The runtime supports two bounded operations: read public
metadata and sign a message up to 4 MiB. Read-only workspace participation gets
no descriptor. Closing the application closes its authority.

## Storage and migration

Legacy schema-1 records contain a 32-byte share. Migration validates that a
single-device share derives the recorded public key before storing anything.
It then prefers the freedesktop Secret Service through `tilo-secrets`' D-Bus
backend. Existing `secret-service-v1` records can still read items older builds
stored under `tilo-purpose` and `tilo-identity-key`: a missing current item falls
back to those exact attributes without copying or re-labeling the credential.
A refused current lookup remains a refusal. If no usable service exists during
initial provisioning, Tilo falls back to `identity/secrets/<public-key>.key`, mode 0600,
inside mode-0700 user-owned directories. Sandboxes cannot name or mount this
fallback. Settings labels it **Private session file + app-scoped signing
broker**; it is intentionally not described as encrypted or hardware-backed.

Metadata replacement uses an exclusive no-follow temporary file, fsync,
rename, and directory fsync. Record and secret reads open once with
`O_NOFOLLOW|O_CLOEXEC` and validate the open handle's type, owner, permissions,
and size. An advisory per-user lock serialises import, migration, activation,
and removal. Imports reject a second record with the same public key. Older
duplicate records share one provider locator, so removing one retains the
secret until the last reference is gone.

The `SecretProvider` trait is the TPM/hardware-backed seam. A provider must
store, load, remove, report its backend id, and report `HardwareBacked` only
after it actually sealed the secret. The default resolver deliberately refuses
`tpm2-sealed-v1` records when no TPM provider is installed; detecting
`/dev/tpm*` alone is never presented as protection.

## Compatibility and limits

`runtime_identity::load()` prefers `TILO_IDENTITY_BROKER_FD`. The old
`TILO_USER_IDENTITY` path remains for unsandboxed development, but the Tilo
launcher never sets or mounts it. Existing application call sites keep the same
`SigningIdentity` API.

Automated tests cover protocol bounds, signature verification, one-shot file
descriptor ownership, migration without secret bytes in metadata, symlink and
permission refusal, duplicate imports, shared-secret removal, active/final
record protections, and prevention of migration/chmod outside the canonical
identity store. They do not prove a particular desktop keyring is unlocked or
exercise a hardware TPM: those require live-session and hardware test plans,
and Settings reports the software fallback until those tests actually succeed.
