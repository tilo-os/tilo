# Tilo Secret Service

`tilo-secret-service` is a per-user D-Bus-activated service that owns
`org.freedesktop.secrets` (the API libsecret speaks: Chromium, 1Password,
`secret-tool`) and backs the portal's Secret interface for sandboxed apps.

## Opening

The vault, `~/.local/share/tilo/secrets/vault.tseal`, is a `tilo-seal`
envelope. Its key is derived from the person's own key — the private half of
the identity pair in their session's identity tree — their random salt and
their public identity. It is therefore *their* vault, not this machine's: the
same person's key on another device derives the same key, and no device key
opens it. Sign-in is what releases it, so the store opens with no prompt of
its own and nothing before sign-in can read it. Until that key is available,
or if the vault belongs to another person or is damaged, the store is locked:
operations answer `IsLocked`, `Unlock` reports nothing unlocked and offers no
prompt, and the file is never replaced.

A vault an earlier build sealed under the device identity is copied once,
whole, under the person's key the first time both are available; the copy is
written beside it, read back and verified, and only then renamed over the
old file, so a failure at any point leaves the old vault the store. Item and
collection ids are carried over unchanged. A machine that cannot yet release
either key relays the ciphertext untouched.

## Who is asking

Callers are never identified by a name they supply. The service reads the
bus peer's process: a Flatpak (cgroup scope and `.flatpak-info` agreeing), a
root-owned executable under `/usr` (`binary:<path>`), or anything else
(`local-process:<path>`). Interpreters are never `binary:`. A process that
loaded the device key (the compositor) is non-dumpable, and the kernel
refuses its executable to the service; such a caller first proves on its
connection that it holds the device key (`so.tilo.Secrets1.CallerChallenge`
and `ProveCaller`) and names its executable, which then stands in for the one
the kernel will not show, for that connection only. Sandboxed Tilo
apps reach secrets through the portal, where the compositor vouches for them
(`sandbox:<package id>`). Items belong to the identity that made them; items
imported from GNOME Keyring are readable by system identities only. Every
identity is recorded on first use and can be revoked or forgotten:

    tilo-secret-service apps list --json
    tilo-secret-service apps revoke <identity>
    tilo-secret-service apps allow <identity>
    tilo-secret-service apps forget <identity>

## Import from GNOME Keyring

The login keyring can be read only while unlocked, which happens at a
password login. At service start, before it claims the bus name, the service
asks the PAM-started `gnome-keyring-daemon` (found through
`GNOME_KEYRING_CONTROL`) to serve the Secret Service, reads the login
collection, stops that daemon and records the import in the vault. It runs
once; at fingerprint or automatic logins it finds the keyring locked and
tries again next time.
