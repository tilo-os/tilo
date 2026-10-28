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
cannot name it by default, and a filesystem grant cannot hand it over either:
the launcher refuses a grant at or inside `$XDG_CONFIG_HOME/tilo`, and a grant
containing it (the whole home directory, or `~/.config`) shows that directory
as an empty read-only tmpfs. Inside a writable grant every directory on the way
there is bound onto itself, so the app cannot move the real store aside and
plant another for Tilo to read; a writable grant holding a symlink on that way
is bound read-only. The same masking covers the accounts vault key, the
publisher trust and revocation lists and every other file under
`$XDG_CONFIG_HOME/tilo`, and a `TILO_SECRETS_DIR` store
(`crates/packaging/launcher/src/capabilities.rs`). On macOS the Seatbelt profile
denies every store outright after every grant, and denies writes to each
directory and symlink on its way, to the same effect
(`crates/packaging/launcher-macos/src/policy.rs`). Settings labels it **Private session file + app-scoped
signing broker**; it is intentionally not described as encrypted or
hardware-backed.

On a Tilo image the Secret Service is `tilo-secret-service`
(`os/usr/share/doc/tilo/secret-service.md`). Its store is the person's own:
it is sealed under a key derived from their identity key pair, their random
salt and their public identity, and that private half is released by their
sign-in. It therefore opens after any sign-in method (password, fingerprint,
automatic login) and never prompts; GNOME Keyring is read once at a password
login for import and serves nothing. The machine's device identity
(`/var/lib/tilo/device-private`, created at early boot and readable through
the `tilo-identity` group) is what a caller proves, never what seals the
vault. So the store is not the machine's: the same person's key on another
device derives the same store, and losing or replacing the device does not
lose it.

Because the vault is the person's, it is locked whenever their key is not
available, when it belongs to another person, or when it is damaged. Then a
new identity key falls back to the file store above and Settings says so,
while a key already in the Secret Service fails to load: it is neither
re-keyed nor moved to a file, and the store file is never replaced, so the
key loads again once the person's key is back. A vault an earlier build
sealed under the device identity is copied once, whole, under the person's
key when both are available (written beside it, read back, verified, then
renamed over), so an upgraded machine keeps what was already stored.

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

### Callers the store cannot examine

The store names each caller by reading `/proc/<pid>/exe`. Loading the device
key makes a process non-dumpable (`tilo_user_identity::persist`), and the
kernel then refuses that link to every other process of the same user
(`EACCES`: only `CAP_SYS_PTRACE` passes, which nothing in the session holds,
and the store's unit has no capabilities at all). The compositor
loads the device key at startup and stays non-dumpable, so the store could
not examine it: every identity it provisioned fell back to the file store
(`broker-file-v1`, Settings showing **Private session file**), MLS endpoint
state (which has no file fallback) stayed off, and its portal Secret backend
was refused as a store manager.

The compositor stays non-dumpable. Instead, a process that holds the device
key proves it, on each bus connection, to the store:

1. Reading the device key (`persist::read_device_key`, behind every device
   key loader) arms the process with `CallerKey = HKDF-SHA256(device secret,
   info = "tilo-seal/v1/store-caller-proof")` (`tilo_seal::secret_service`).
   Only that derived key is kept for proofs.
2. While the process is non-dumpable, `tilo-secrets` asks the store for a
   fresh 32-byte challenge (`so.tilo.Secrets1.CallerChallenge`) before it
   opens a session, and answers it (`ProveCaller`) with its executable,
   read from its own `/proc/self/exe`, and `HMAC-SHA256(CallerKey, challenge
   ‖ its unique bus name ‖ that executable)`. The portal backend proves on
   its own connection the same way when the store refuses it.
3. The store derives the same key from the device key — not from the key
   the vault is sealed under, which is the person's — spends the challenge,
   and checks the proof in constant time. If it
   holds, the named executable stands in for `/proc/<pid>/exe` for that one
   connection, but only while the kernel refuses the link (`EACCES`): a
   readable link always names the caller, and a link that is gone (the
   process exited) is never replaced by a proof. The executable is then
   classified exactly as a link would be: `binary:` only for a root-owned,
   non-writable executable under `/usr` that is no interpreter, otherwise
   `local-process:`; a replaced (`(deleted)`) executable is refused. The
   proof is forgotten when the connection leaves the bus or the store
   restarts; `tilo-secrets` then proves again and retries the refused call
   once.

Why this is at least as strong as reading the link:

- **It grants nothing the prover does not already have.** The device
  secret, with the person's salt in their own data directory, is what the
  store's own key derives from. Whoever can make the proof can already
  decrypt the store's file, whatever executable it names. A process without
  the device key, such as any sandboxed app or a same-user process that only
  turned itself non-dumpable, has nothing to prove with and stays refused.
- **It cannot be replayed or moved.** The challenge is random, single-use and
  issued to one unique bus name; the proof covers that name, and the bus
  never hands a unique name to another connection. A proof seen by a bus
  monitor is useless on any other connection.
- **Nothing a same-user process can set is trusted.** Not `comm`, not the
  cgroup (a delegated user manager lets the person's processes join any of
  its cgroups, and apps the compositor launches share its scope), not the
  environment.
- **PID reuse does not apply.** The proof binds the bus connection, not a
  PID; the store still takes the PID from the bus daemon's credentials only
  to check the user and to read the link first.

Rejected alternatives: a dumpable relay process that makes the calls for the
compositor would itself be a confused deputy (any same-user process can
start it and ask), and would carry the identity root secret through a
dumpable process; caching an identity read while the compositor was still
dumpable would be lost whenever the store restarts, since the compositor
cannot become dumpable again; identifying by cgroup or systemd unit is
spoofable as above, and the compositor is not a unit of its own.

Residual: like an executable read from `/proc`, a named executable says what
was executed, not what code runs in it; the store already accepts that for
every caller (`LD_PRELOAD` into a system binary). A device-key holder that
`exec`s another program while leaking its bus connection across the `exec`
would pass its proof on; Tilo's bus clients open connections close-on-exec.

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

The same test runs clients in child processes, against a store, both
without capabilities (as the store's unit `CapabilityBoundingSet=` and a
person's session run them). A dumpable client is the control: the store
examines it and keeps its key. Clients that turned themselves non-dumpable
with no device key, or after reading another machine's device key, are
refused and fall back to the file; with the control, that refusal is what
shows the store really cannot read a non-dumpable executable. One that read
this machine's device key, as the compositor does, gets `secret-service-v1`
and no key file, signs, stays non-dumpable, and signs again after the store
restarts under it. A second test serves the store's management interface
with that test binary as portal host: a dumpable host is answered by its
executable, a non-dumpable host is refused `SandboxSecret` until its
connection proves the device key, a proof from another device's key is
refused, and a proof covers only the connection that made it.

Nothing exercises a hardware TPM: no TPM provider exists, so no identity is
ever reported as hardware-backed.
