# Importing cloud files once

Dropbox and Google Drive accounts connected in Settings appear in Files as
ongoing synced locations under `~/Remote/<account>`. Remote changes appear in
these folders and edits made there are uploaded to the provider.

For an independent local copy, select an entry in either location and choose
**Import once to Downloads** in its context menu. The destination is the user's
XDG Downloads directory and must already exist. The entry can be a file or a
folder. Name collisions get a new name rather than replacing existing files.
The shared action chooser also offers **Import once to local folder…** on the
entry, with a destination-folder parameter.

Automation uses the same entry action, `import-once`, with
`{"folder":"/absolute/local/destination"}`. Discover the entry's resource id
through the Files action directory; for example:

```sh
tilo-control actions list
tilo-control actions invoke PROVIDER ACTION_ID --args-json '{"folder":"/absolute/local/destination"}'
```

The action resolves only a currently visible Dropbox or Drive entry. It uses
Files' ordinary queued, undoable copy operation. Reads through the cloud mount
hydrate bytes on demand; the result is a regular local copy, not a cache pin,
shortcut to the mounted file, or another synced mount. Later cloud changes do
not change it, and edits to the imported copy do not upload to the provider.
Imports into known synced cloud folders, including symlink aliases, are refused.
No filesystem grant or sandbox permission is added by importing. A destination
outside the workspace's existing access still requires the usual file grant.

## Google-native documents

Drive-native documents do not have ordinary downloadable file bytes. The mount
projects them as provider link files containing the document id, MIME type,
web URL and available export extensions. Import preserves that honest link
representation; it does not claim to have downloaded an editable local document.
The link may still open the live provider document. Download/export the desired
format from the provider when a self-contained document is needed.

## Account connection

In **Settings → External Accounts**, add Dropbox or Google Drive and complete
browser authorization. For Google, choose **Connect files and Gmail** to request
both file and mail permissions. The ordinary connect action requests files only.
After combined consent, Settings saves the Drive location and a Gmail account;
Mail follows the shared person-account record and shows the new Gmail account,
including in an already-open session window. Gmail access and the mailbox identity are
validated before saving either account; denied Drive or Gmail permission does
not leave a partially connected pair. A Gmail row's **Sign in again** action
renews mail authorization and checks that Google returned the same mailbox.

Google sign-in requires a configured OAuth client and provider approval for the
requested scopes. Gmail must be available to the Google account, and the client
project must have the Gmail API enabled. These requirements cannot be supplied
by a simulator or bypassed by saving a nominal Mail account.

Importing does not disconnect, pause or reconfigure the source account. The
original cloud location keeps syncing. Account setup and credential custody
remain in Settings; Files never reads a provider credential. An account must be
connected and mounted before Files can import from it.

## Verification

`crates/system/fs/tests/cloud_mount.rs` exercises the production cloud volume's
hydration, remote-change polling and local-write upload against the Dropbox and
Drive fake HTTP providers. The Files test
`one_time_cloud_import_is_an_independent_copy_through_the_action_door` invokes
the published action over a mounted-folder fixture and verifies recursive
copying, unchanged copies after source edits, independent local edits, native
link preservation and rejection of a symlink alias to a cloud destination.
A second test redirects a destination alias after admission and verifies the
queued copy retains the validated local destination.

`apps/settings/tests/xdg.rs` includes the cloud-storage acceptance tests for
browser sign-in, credential handoff, mount creation, writes and removal.
`apps/mail/tests/google_settings.rs` exercises combined Google consent through
Settings and consumption/refresh by Mail, including partial or denied consent.
Settings sign-in tests cover live-record rollback and Gmail reauthentication.
Mail's `already_open_mail_adopts_settings_google_consent_on_sync` regression
checks account appearance in an already-open window through production sync.
All provider-account tests use isolated local HTTP fixtures; they do not log
into a real person's Dropbox or Google account.
