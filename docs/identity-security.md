# Identity signing boundary

Tilo applications do not receive private identity material, and the launcher
does not hold any. For each authored launch, `tilo-launcher` resolves the
selected identity on the host and reads only its public fields
(`runtime_identity::load_metadata_for_launch`): the record is validated as
usual, but its secret provider is never contacted, so no secret is loaded. It
opens exactly one Unix socket pair, serves that metadata on its end, and places
the app end at file descriptor 15 (`TILO_IDENTITY_BROKER_FD`). That broker
answers public metadata in frames of at most 4 MiB and refuses every `sign`: a
root signature over the right 32 bytes is a device certificate or a revocation,
so no application ever obtains one (`runtime_identity::broker::serve_metadata`).
A store record whose key was never secured still fails the launch and names the
Settings step that secures it.
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

On a Tilo image the Secret Service is `tilo-secret-service`
(`os/usr/share/doc/tilo/secret-service.md`). Its store is sealed under a key
derived from the machine's device identity (`/var/lib/tilo/device-private`,
created at early boot and readable through the `tilo-identity` group), not
from a login password. It therefore opens after any sign-in method (password,
fingerprint, automatic login) and never prompts; GNOME Keyring is read once at
a password login for import and serves nothing. It is locked only while the
device identity is unavailable, or the store belongs to another device
identity or is damaged. Then a new identity key falls back to the file store
above and Settings says so, while a key already in the Secret Service fails to
load: it is neither re-keyed nor moved to a file, and the store file is never
replaced, so the key loads again once the device identity is back.

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
of migration/chmod outside the canonical identity store.

`runtime-identity`'s own test builds always use the file store. The store a
real session chooses is proven by `crates/system/secret-service/tests/identity_custody.rs`,
which runs the production identity and vault code against the built
`tilo-secret-service` on a private session bus. With no Secret Service on the
bus, a new key goes to the 0600 file, the record names `broker-file-v1`,
Settings' protection is **Private session file**, and a sealed vault keeps its
key in a local file ("Key file on this device, no keyring"). With the service
open it is preferred: the record names `secret-service-v1` and holds no key
byte, no key file is written, the store file holds no plaintext key, and the
vault key goes to the system store. After the service restarts, a client that
kept running signs on its next call. While the service is locked, a secured key
does not load and is not re-keyed or moved, the store file is unchanged, and a
new key falls back to the file. Once the device identity is back, the original
key signs again. A truncated record fails its launch with a named error.

Nothing exercises a hardware TPM: no TPM provider exists, so no identity is
ever reported as hardware-backed.
