# Authentication custody

Bearer tokens, Basic credentials and API keys live in the app's host-provided
`SecretStore`. Saved requests and action payloads contain only an opaque
`SecretRef`; a reference is not usable on another host unless that host has
explicitly provisioned the corresponding secret. Sending with a missing, locked
or invalid secret fails rather than silently sending without authentication.
Native standalone launches select the platform store at their composition root;
host-mounted components use `HostContext::secrets()`.

On opening an older document, migration seals the visible authentication values
before replacing their current fields, including legacy JSON record blobs.
Failed protected writes leave the document unchanged so unlocking and retrying
can preserve the credentials. Plaintext auth values cannot be serialized into
new collaborative records or actions, and debug output redacts auth and outgoing
requests.

**This migration does not erase Automerge history.** Old changes, peer replicas,
exports and backups can still contain the original credentials. Rotate affected
credentials and replace/reset the shared document on every replica, disposing of
old exports/backups according to their retention policy. Field replacement alone
is not secure erasure.

The request editor is also a general HTTP payload editor. URLs, manually entered
headers, request bodies and environment variables remain collaborative data;
do not put credentials there. This change protects the dedicated authentication
fields, not arbitrary user-authored payloads or server responses. References are
app-owned material, not a Passwords broker consent grant.

In a browser, authentication writes remain pending until IndexedDB acknowledges
the transaction. The editor retries on later idle frames; it never puts a
reference into shared state on the strength of an in-memory write. Quota,
permission or crypto errors leave credentials local and require recovery before
saving. Multi-field migrations retain each acknowledged reference locally across
retries and publish document edits only once every field is ready; retrying must
not consume the same acknowledgement twice or start another write for it. A save
action submitted while custody is pending can be retried after the pending write
completes.

Browser custody is not an authentication boundary between same-origin scripts:
those scripts can use the origin's non-extractable keys. Requests are also
subject to browser CORS and forbidden-header rules. A backend proxy must
authenticate the caller and authorize the destination; neither a secret id nor
an app namespace grants access to that backend.
