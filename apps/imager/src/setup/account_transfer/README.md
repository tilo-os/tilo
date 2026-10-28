# Account transfer backend

`discover(&SourceRoots { home, config })` examines only roots the person has
chosen to migrate. `config` is the source XDG config directory, commonly
`home/.config`. Results are local metadata, never credential values. The source
paths also identify private profile data to retain in the full home transfer.
An application/browser profile is evidence of profile data, not proof of a
currently authenticated session.

Implemented Linux sources:

- Existing local Tilo connections for supported cloud providers and Spotify.
  Discovery reads reviewed account metadata only; credentials stay private.
- GNOME Online Accounts: configured Google, Microsoft, Nextcloud and WebDAV
  accounts and presentation identities.
- Nextcloud desktop: multiple configured servers and login names.
- rclone: Drive, OneDrive, Dropbox, Nextcloud and generic WebDAV remotes.
- Native and Flatpak Slack, Discord, Spotify, Firefox, Chromium and Chrome
  profiles; native Brave profiles.

The config readers intentionally allowlist metadata keys. Discovery does not
open browser cookie/login databases, app token stores or the system keyring.
Encrypted rclone configuration, unreadable data, oversized files and symlink
escapes are reported rather than called an empty successful scan.

`Connector::new(root)` takes **Tilo's** destination config root, normally
`target_home/.config/tilo`. It uses the existing canonical `accounts` store and
sealed vault; credentials never belong in installer resource state, actions,
status responses or migration manifests.

- On Unix, selected ordinary rclone Google Drive, OneDrive and Dropbox refresh
  grants are validated with their original installed-app client registration.
  That registration remains sealed per grant for future canonical refresh, independent
  of Tilo's configured OAuth client. Imported detach never revokes the source session.
  Recognized Drive folders/shared drives and OneDrive drive/folder IDs are retained.
  Custom endpoints, service accounts, client-credentials flows, sovereign/non-common
  tenants and unsupported scopes require reconnection. Discovery is metadata only;
  missing or rejected credentials offer explicit sign-in, not a usable-login claim.
  Provider acceptance still requires live qualification; rclone's shared Google
  registration is being retired during 2026.
- `import_rclone(roots, candidate)` also rechecks a selected WebDAV/Nextcloud remote
  and extracts its reusable obscured app password directly into the sealed
  vault. The source DAV files-root URL is retained as generic WebDAV (including
  Nextcloud DAV roots/subfolders), rather than appending a second Nextcloud DAV
  path or guessing the server user ID. The AES-CTR decoder implements rclone's public format. It never spawns
  a credential-bearing command. Source bytes remain unchanged.
- Linux `import_nextcloud(roots, candidate)` rechecks the exact selected native
  account and reads its modern account-ID HTTP credential through the owner
  Secret Service. The upstream QtKeychain key and attributes are matched exactly;
  selection metadata is checked again after lookup before canonical Nextcloud
  custody is written. Repeated imports are idempotent, source bytes stay unchanged,
  and deleting the copied grant never revokes the original. Discovery alone does
  not inspect the keychain or establish a usable login. Missing/ambiguous entries,
  legacy keys, direct KWallet-only custody and client certificates require sign-in.
- `import_tilo(source_tilo_root, account_id)` imports an already-Tilo local
  account and its available explicit-root vault credential. Accounts owned by
  other collaborators are excluded. A source with unavailable device/system
  key custody requires reconnection; encrypted bytes alone are insufficient.
  Guided selection uses `import_tilo_reviewed`: it checks the reviewed metadata
  fingerprint before and after opening source custody, reads the existing vault
  without creating keys or changing source permissions, and reseals the selected
  grant into destination custody. Durable copied provenance remains sealed through
  refresh and transport, so removing that copy does not revoke the source.
  Owned prepared-stage handoff preserves fresh versus copied provenance rather
  than marking every newly authorized connection as a source copy.
- Imported account IDs are stable. Retries succeed only when existing target
  account metadata and its privately compared credential match exactly (or a
  sealed original-source fingerprint matches after legitimate target refresh);
  unrelated target entries are refused.
- `begin_oauth`/`finish_oauth` connect Google Drive, OneDrive or Dropbox through
  shared PKCE/state validation and shared provider storage resolution.
- `begin_spotify`/`finish_spotify` connect the Spotify library into the
  exact `music.spotify` vault slot used by Tilo Music. Native streaming is a
  separate grant.
- Guided iCloud setup asks for Apple Account, then password, then the trusted-device
  verification code when Apple requires it. Invalid codes can be retried; Back
  restarts sign-in. Cancellation and controller drop compensate newly granted
  sessions on the retained private worker. Passwords, challenges and session
  trust never enter shared state or action receipts.
- `icloud_sign_in`/`icloud_verify`/`finish_icloud` reuse Apple sign-in and
  two-factor handling, sealing session trust without persisting the Apple ID
  password. Provider restrictions remain actionable domain errors.
- `finish_nextcloud` accepts a newly minted Nextcloud Login Flow v2 result;
  `connect_nextcloud_app_password` retains existing explicit app credentials
  without marking them for cancellation revocation. `connect_webdav` accepts
  a private local app-password buffer. Both persist configurations understood
  by the existing DAV storage adapter. The account page now guides new and
  discovered DAV/Nextcloud connections through server, login and app-password
  pages using shared secret inputs and a secret semantic field action. Saving
  a new connection first checks the exact files root with a bounded, read-only
  DAV request. Authentication refusal, an unavailable endpoint, and a root
  without file access remain distinct retriable results. Copied account grants
  remain offline during migration and preserve their original credentials. Nextcloud
  browser Login Flow v2 is also guided: after entering the server, the person
  explicitly chooses browser sign-in or their existing app password. The
  canonical begin/wait flow runs on a private worker, and only the login URL
  reaches the shared URI opener. Poll tokens stay private; cancellation/drop
  retains the stage through compensation of a returned new app password.

Provider grants and browser sessions have separate consumers. This account
module imports supported provider grants into canonical account custody. The
adjacent `service_sessions` module captures explicitly selected Slack/Discord
browser or application sessions, seals them into PreparedSetup, and imports them
into protected browser-session custody. Browser's `OpenServiceSession` action
consumes those sessions through the shell's private broker. Installing session
material locally does not prove the provider will accept it on another OS/device;
expired or rejected sessions require sign-in. Configuring/sealing a password is
not a provider-side authentication test.

Common application authentication limits:

- Slack/Discord: selected Firefox/Chromium/Slack cookie stores and Discord
  origin storage have source extraction, authenticated transport and a Browser
  consumer. Discovery alone does not inspect them or establish an active login.
  Tilo Chat has external channel links, without Slack/Discord API backends.
- Spotify: new Tilo OAuth sign-in is available; existing **Tilo** catalogue and
  streaming OAuth slots can be selected and resealed. Official desktop profiles,
  browser cookies and third-party OAuth refresh tokens do not become those slots.
  Pinned librespot supports reusable `Credentials` caches, but Tilo's current
  native playback consumer accepts only MusicStreaming OAuth and separately
  verifies catalogue/playback Web API identities. Cache import remains unsupported
  until a typed reusable-session adapter can satisfy that identity gate.
- Browser profiles: selected bookmarks, history and folder structure have
  separate source parsing and Browser import actions. Whole-profile preservation
  does not establish a login. Session import supports specific source formats;
  unsupported encryption/custody and rejected sessions require reconnection.

Sources for the external formats:

- https://gnome.pages.gitlab.gnome.org/gnome-online-accounts/configuration.html
- https://github.com/nextcloud/desktop/blob/master/src/gui/accountmanager.cpp
- https://github.com/nextcloud/desktop/blob/master/src/libsync/creds/abstractcredentials.cpp
- https://github.com/frankosterfeld/qtkeychain/blob/main/qtkeychain/libsecret.cpp
- https://docs.nextcloud.com/server/latest/user_manual/en/desktop/configfile.html
- https://github.com/rclone/rclone/blob/master/fs/config/obscure/obscure.go

These provider config formats are private/versioned. Unknown layouts should be
extended with fixtures, never guessed into credential access.

`actions::AccountActions` is the callable device-local action backend for the
journey. Create it with the reviewed source roots/inventory. The destination
home comes from the installer's existing reviewed destination; no extra location
chooser or app launcher is introduced here. Run `import_selected` or
`import_tilo` after full home copy settles, or explicitly `begin` a provider
connection and open its public consent URL through the host portal. `complete`
starts the private exchange worker. `status` yields only running/completed/failed
receipts and account IDs. Uncompleted authorization handles may be cancelled;
credential writes are not represented as cancellable once started.

A same-computer handoff does not put GOA tokens or vault bytes on media. Its
private source journal carries the exact selected GOA identities and the
services the source enabled; after first boot discovery is repeated under the
retained UID 1000 Home and only that selection is revalidated. The destination
then creates canonical pending Files, Mail, Calendar and Contacts metadata for
the selected services. A destination mailbox that already exists is reused
without replacing its configuration or credential, so the same identity from
GOA and Thunderbird does not create two mail accounts or overwrite a working
sign-in. Offline means a pending row and an individual reconnect, never a
failed import. Existing destination credentials that the destination already
holds are left untouched; a source grant is never adopted as root or as proof
that Mail or Calendar has the corresponding scope.

Selected Thunderbird address books and calendars are retained byte for byte and
also parsed into canonical vCard/iCalendar. Contacts and Calendar read that
canonical data through their own import inbox when opened, so the person sees
real contacts and events instead of an unused copy below `mail-import`.
Thunderbird's SQLite local stores are read only from a private temporary copy;
an unreadable or unsupported sidecar publishes no fake contacts or events.

The backend refuses copied destination config/vault symlinks and malformed
existing account stores. Parent coordination must keep home copying and account
writes sequential, and retain its destination directory custody throughout.

`journey::accounts::Accounts` supplies the account decision pages and private
controller. Choosing or rendering an account never starts authorization; the
explicit Connect action invokes the shared URI portal. The controller stages
credentials in an owned private temporary home outside the source, retains
worker custody until they finish, and records only safe receipts in its public
projection. Back cancels browser authorization; a completed authorization that
races cancellation is revoked best effort and removed from the private stage.
Closing during token exchange also retains private worker custody through
new-grant cleanup; accepted prepared receipts transfer that responsibility to
the retained selection. Failed cleanup must be retried before continuing. Back
during an already-started saved credential import preserves its completed
selection.

The transfer controller retains `prepared_handle()` without consuming the
account decisions, then calls `merge_into` after home transfer into the
destination settles. That method reseals selected
accounts into destination custody; source and unrelated destination accounts
remain protected. Retain the handle on failure for retry. Portable media needs
authenticated account transport or reconnection on the destination: neither
staging paths nor vault keys belong in the public installer profile.

Portable selected accounts have a separate transport contract:
`Connector::seal_selected_accounts(selected_ids, &SymmetricKey,
nonempty_setup_binding)` reads only explicitly selected canonical account IDs
and returns `SealedAccounts`. The guided installer's ExportPreparedSetup effect
obtains exact IDs from its retained consented selections and exports the
versioned manifest with canonical identity and password-wrapped account artifacts.
`Connector::import_sealed_accounts` authenticates all entries and
preflights all target conflicts, then reseals into the target canonical vault.
The target caller must supply a reviewed, privately owned destination through
existing destination custody. Exact metadata/secret retries finish partial
vault-before-metadata persistence; differing credentials or accounts are refused.
Source vault ciphertext and source vault keys are never a transport format.

The symmetric envelope uses canonical `tilo-seal`, its MigrationPackage label,
and the distinct `tilo-prepared-accounts/v1` domain plus the setup binding. It is
bounded to 64 accounts and 8 MiB of plaintext. Exported credential copies and
serialized plaintext buffers are kept in private memory and cleared after use; only sealed
bytes can leave this backend. Non-local contributed accounts are excluded.

`Connector::seal_selected_accounts_with_password` now generates a fresh account
content key and wraps it using canonical password sealing under the distinct
`tilo-prepared-account-key/v1` domain plus explicit operation binding.
`PasswordSealedAccounts::{as_bytes,from_bytes,read}` supports bounded artifact
handoff, and `Connector::import_password_sealed_accounts` opens and reseals it.
Version 1 accepts only the canonical default Argon2 policy before derivation,
refusing untrusted resource-expensive KDF parameters. The transport password
uses the existing migration policy: at least 15 UTF-8 characters.

`Connector::export_selected_accounts(selected, path, password, binding)` creates
an actual owner-private artifact using a pinned private native parent. It writes
ciphertext only to a private temporary file, syncs and checks readback, then
atomically links the final new name without replacing existing files or following
symlinks. Temporary ciphertext names are removed. Output within or above the
source config root is refused; the installer must additionally retain its full
source-home grant when choosing a location outside that home. Native directory
persistence support is required. `Connector::import_selected_accounts` reads a
bounded private file and invokes the authenticated canonical import.

`Connector::import_selected_accounts_matching` checks the complete authenticated
ID set against an outer manifest before any destination writes. Missing, extra
or duplicate selections fail closed. `PasswordSealedAccounts::verify_selection`
performs this check without opening target custody. The in-memory matching
import supports the Linux `prepared_setup::PreparedSetupReview` consumer, which
authenticates a Purpose::PreparedSetup package, quarantines exact manifest/
identity/account files, and verifies both inner artifacts before explicit
identity or account adoption. Imports run as the target user; root must not
provision another user's canonical account vault with this API.

Boot installer/first-login invocation and media discovery remain caller work;
these backend imports do not themselves activate a persona or adopt an OS.
Never put an unwrapped key, credential or source vault key in an InstallProfile/
resource/action response. The existing home archive is not a transport format
for prepared account grants or device-bound keys.

Common-app authentication gaps remain explicit: Slack and Discord profile
presence does not prove an active login. Their selected session material is
handled by `service_sessions`, and selected browser bookmarks/history by
`browser_transfer`; neither is imported by this account module. Local session
installation and provider acceptance require distinct verification. GNOME Online
Accounts and ordinary rclone cloud OAuth refresh grants retain their original approved client
registration in canonical sealed custody; unsupported registrations/flows require
reconnection. Selected reusable
rclone DAV app passwords, recognized Linux Nextcloud Secret Service HTTP
credentials and supported own Tilo vault credentials are actually extracted. Spotify connects through its own Tilo
library OAuth grant; copying a native Spotify profile does not adopt that grant.

The OS ships the headless `tilo-prepared-setup` consumer at
`/usr/libexec/tilo-prepared-setup` through the component manifest. It accepts only
explicit `--package`, `--quarantine`, `--identity-dir`, and `--config-root`
native absolute path grants. Raw UTF-8 password bytes arrive over stdin, bounded
to 4096 and held in zeroizing memory; no password appears in argv, environment,
or its receipt. The caller closes stdin and supplies a fresh quarantine on
retry. Delivered public-media ciphertext must first be copied into the target
user's private grant and checked against its expected delivery receipt.
Both target grants are checked before adoption. Success stdout is only
`{"identity_public_key":[...],"account_count":N}` after identity and exact
selected accounts persist; all failures use generic stderr, no success receipt,
and preserve the encrypted package/quarantine for explicit retry. The helper
never activates a persona or mints a replacement identity; shell onboarding
owns canonical activation, device provisioning and completion.

Copied OAuth grants/original-client registrations and copied iCloud sessions now
use protected wire kinds. Legacy custodians refuse them rather than silently
ignoring provenance or using a different client. Copied DAV/Nextcloud passwords
are held inside the protected sealed token with an empty outer password; only
updated canonical custody (including the Settings DAV probe) unwraps them.
Fresh Tilo OAuth/iCloud grants and ordinary non-DAV password representations keep
legacy compatibility. An old target therefore requires an updated consumer to
use these copied connections; refusal is safe, not successful migration.

PreparedSetup now authenticates a protocol-1 requirement only for the actual
selected protected grants. A stripped/mismatched requirement refuses before
adoption. Direct internal staging checks the authenticated selected bundle's
consumer receipt, which the exporter re-attests from the exact verity-required
readonly composefs deployment and unchanged component bytes. The target-user
prepared helper independently runs the fixed image-owned consumer verifier
before identity/account writes; a newer staged importer is not consumer proof.
The verifier checks the actual credential helper and signed SDK-linked probe.
Fresh legacy grants need no protected protocol requirement. Desktop distribution
receipt publication and pre-write raw/USB target admission still need separate
qualification; the installed consumer gate must not be mistaken for those proofs.


## Detected Spotify profiles

Native `config/spotify` and Flatpak `com.spotify.Client/config/spotify` profiles
remain profile evidence, not reusable Tilo OAuth credentials. Guided source
selection opens the existing Spotify purpose chooser with that exact candidate
retained. It offers only configured library/playback grants, including a
playback-only registration. Back preserves the candidate until returning to the
source list; stale or withdrawn configuration refuses the semantic action before
OAuth work. The page explicitly says the existing session will not move. A new
connection still uses canonical OAuth custody and the existing verified Spotify
identity checks; this route does not prove a migrated session or provider acceptance.

[librespot 0.8.0 cache implementation](https://github.com/librespot-org/librespot/blob/v0.8.0/core/src/cache.rs)
uses a caller-configured credentials directory and a `credentials.json` file.
Its [Credentials type](https://github.com/librespot-org/librespot/blob/v0.8.0/core/src/authentication.rs)
contains an optional username, numeric authentication type and base64 authentication
data. That is a librespot format, not evidence that either detected official
Spotify profile uses that format or contains an OAuth refresh grant. Spotify's
[authorization documentation](https://developer.spotify.com/documentation/web-api/concepts/authorization)
describes OAuth flows; no verified official desktop-session export format is
admitted by this backend.

Reusable-session migration therefore still needs an independently recognized,
reviewed source cache and a typed supported credential adapter, sealed portable
custody, and a native consumer that independently verifies catalogue/playback
account identity. Arbitrary cache paths, opaque desktop cookie/token blobs and
librespot authentication bytes are not imported as OAuth grants. Existing local
Tilo Spotify imports remain the supported reusable-credential path.
