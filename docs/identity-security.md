# Identity signing boundary

Tilo applications do not receive private identity material. For each authored
launch, `tilo-launcher` resolves the selected identity on the host, opens exactly
one Unix socket pair, keeps its end, and places the app end at file descriptor 15
(`TILO_IDENTITY_BROKER_FD`). That broker answers public metadata in frames of at
most 4 MiB. It refuses `sign` for every record it serves today: a record whose
local secret is the person's root (group) key never signs for an application,
because a root signature over the right 32 bytes is a device certificate or a
revocation (`runtime_identity::broker`, `SigningIdentity::signs_with_root_key`).
Application signatures come from the identity service (`tilo-identity-host`,
file descriptor 31), which signs with this device's enrolled key and only under
a grant naming the app, room and action. Read-only workspace participation gets
no broker descriptor; the identity-service connection is still attached when the
launch names a workspace, and its signing refuses without that grant. Managed
(enrolled-agent) launches get no broker. Closing the application closes its
authority.

## Storage and migration

Legacy schema-1 records contain a 32-byte share. Migration validates that a
single-device share derives the recorded public key before storing anything.
It then prefers the freedesktop Secret Service through `tilo-secrets`' D-Bus
backend, looked up by `application=so.tilo.identity` and a per-record
`purpose`. Items that older builds stored under other `secret-tool` attributes
are neither read nor migrated. A refused current lookup remains a refusal. If
the Secret Service does not answer, or fails while a record is being secured
(initial provisioning, sign-in, Settings' Protect, activating an identity),
Tilo falls back automatically to `identity/secrets/<public-key>.key`, mode
0600, inside mode-0700 user-owned directories; `TILO_IDENTITY_FILE_SECRETS`
forces that file store for tests. No sandbox is given this path, so an app
cannot name it by default; a filesystem grant to a folder containing it (the
whole home directory, or `~/.config`) would expose it, as it would every other
file of the person's. Settings labels it **Private session file + app-scoped
signing broker**; it is intentionally not described as encrypted or
hardware-backed.

Metadata replacement uses an exclusive no-follow temporary file, fsync,
rename, and directory fsync. Record and secret reads open once with
`O_NOFOLLOW|O_CLOEXEC` and validate the open handle's type, owner, permissions,
and size. An advisory per-user lock serialises import, migration, activation,
and removal. Imports reject a second record with the same public key. Older
duplicate records share one provider locator, so removing one retains the
secret until the last reference is gone.

The `SecretProvider` trait is the TPM/hardware-backed seam. A provider must
store, load, remove, report its backend id, and report `HardwareBacked` only
after it actually sealed the secret. No TPM provider is implemented: the
default resolver deliberately refuses `tpm2-sealed-v1` records, and no record
reports `HardwareBacked`. Detecting `/dev/tpm*` alone is never presented as
protection.

## Compatibility and limits

`runtime_identity::load()` prefers the identity-service descriptor, then
`TILO_IDENTITY_BROKER_FD`. The old `TILO_USER_IDENTITY` path remains for
unsandboxed development, but the Tilo launcher never sets or mounts it. Existing
application call sites keep the same `SigningIdentity` API.

Automated tests cover protocol bounds, the broker's refusal to sign with a root
key, signature verification, one-shot file descriptor ownership, migration
without secret bytes in metadata, symlink and permission refusal, duplicate
imports, shared-secret removal, active/final record protections, and prevention
of migration/chmod outside the canonical identity store. Test builds always use
the file store, so they do not prove that a desktop keyring is preferred,
unlocked, or that a dismissed prompt falls back as described; nor do they
exercise a hardware TPM. Those require the live-session and hardware test
plans, and Settings reports the software fallback until those tests actually
succeed.
