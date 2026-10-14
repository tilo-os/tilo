# Tilo Secret Service

`tilo-secret-service` is a per-user D-Bus-activated service that owns
`org.freedesktop.secrets` (the API libsecret speaks: Chromium, 1Password,
`secret-tool`) and backs the portal's Secret interface for sandboxed apps.

## Opening

The vault, `~/.local/share/tilo/secrets/vault.tseal`, is a `tilo-seal`
envelope. Its key is `HKDF(device identity key, the person's random salt,
account)`. The device identity is readable by the session however the person
signed in (password, fingerprint, automatic login), so the store opens with
no prompt of its own. Until the identity exists, or if the vault belongs to
another device identity or is damaged, the store is locked: operations answer
`IsLocked`, `Unlock` reports nothing unlocked and offers no prompt, and the
file is never replaced.

## Who is asking

Callers are never identified by a name they supply. The service reads the
bus peer's process: a Flatpak (cgroup scope and `.flatpak-info` agreeing), a
root-owned executable under `/usr` (`binary:<path>`), or anything else
(`local-process:<path>`). Interpreters are never `binary:`. Sandboxed Tilo
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
