# Passwords and app credentials

Passwords is the user-facing inventory for Tilo's encrypted secret store. Titles,
usernames, service origins, notes, custom fields and passwords are all inside the
authenticated vault ciphertext. The plaintext account store never contains them.

New applications should use `accounts::CredentialBroker`, not open `SecretVault`.
Every request names the calling app, item, purpose, and (for web sign-ins) exact
origin. The broker validates the origin before prompting and supports one-time or
session consent. Session grants are memory-only and disappear at logout; users can
revoke an app immediately. The returned password uses `Zeroizing<String>` so its
buffer is cleared when dropped.

The vault is XChaCha20-Poly1305 sealed with a fresh random nonce per write. On a
normal desktop the encryption key is held by Secret Service. A session whose
Secret Service does not answer falls back automatically to an owner-only key
file beside the vault, so anyone who can read that directory can open the vault.
Settings → Accounts says so, and the Passwords app names it "Key file on this
device, no keyring" rather than calling it encrypted storage.
The Passwords app has no network access and does not sync secrets through CRDT state.

Password generation uses OS randomness, rejection sampling (no modulo bias), a
20-character default, and omits commonly confused characters.

The Passwords UI keeps search and edit drafts in process-local state. Reveal is
masked again after ten seconds. Copies are offered through native effect actions
and expire after thirty seconds; Tilo clears only if the clipboard still contains
the copied password, so it never erases newer user content. Editing with a blank
password preserves the existing one, and deletion requires a second explicit
confirmation.
