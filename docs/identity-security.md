# Identity signing boundary

Tilo applications do not receive private identity material, and the
identity's root key never signs for them. For each launch into a workspace,
`tilo-launcher` connects the session's identity service and hands the app one
end, bound by the launcher to that workspace and app before the sandbox
starts. The app reads public metadata (the person's identity key, this
device's key and the certificate the root issued for it) and asks the service
to sign. The service signs with this device's key, after checking that the
certificate is valid, unrevoked and enrolled, and that the workspace lets this
app do what it asks; peers accept the result through the identity → device
certificate → signature chain (`tilo_user_identity::verify_chain`). A device
another device enrolled holds only the identity's public record and runs every
app the same way. Closing the application closes its authority.

The root key stays on the devices that can approve others. It signs only what
must come from the root: device certificates, revocations, and the identity's
backup and recovery. A device without it refuses those and names a device
that holds it.

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

`runtime_identity::load()` uses the inherited identity service. The
`TILO_USER_IDENTITY` path remains for unsandboxed development, but the Tilo
launcher never sets or mounts it.

Securing a record that holds only the identity's public half leaves it as it
is: there is no secret to store, and a provider never stores an empty one.

Automated tests cover protocol bounds, signature verification, migration without secret bytes in metadata, symlink and
permission refusal, duplicate imports, shared-secret removal, active/final
record protections, and prevention of migration/chmod outside the canonical
identity store. They do not prove a particular desktop keyring is unlocked or
exercise a hardware TPM: those require live-session and hardware test plans,
and Settings reports the software fallback until those tests actually succeed.
