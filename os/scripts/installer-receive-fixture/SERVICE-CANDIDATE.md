# Candidate-scoped service qualification

The existing owned `receive.py --service-session-qualification` coordinator
admits the signed Browser/SDK/CEF graph and measured receiving closure before
starting the source lifetime. It replays that admission inside each namespace.
The source's measured Imager may select protocol 2 for this SDK without a prior
service receipt. The receiving helper additionally compares the person and
setup binding from its own decrypted, completed prepared manifest before any
service custody import. Neither scope is an installed consumer capability.

`/run/tilo-service-candidate-admission.json` is created exclusively by root in
the existing isolated lifetime. It binds the source or receiver phase, actual
mount/PID namespace, measured executable, exact graph/closure hashes and (for
the receiver) native-authenticated person/binding. The readable initial PID
namespace witness is a kernel namespace bind, not a JSON assertion. The marker
and witness disappear with the owned namespace; they are never installed,
transported as receipts, or enabled through environment variables. Missing or
invalid admission leaves the normal proof requirement closed.

## Browser semantic observation contract

The receiver exercises the ordinary certified Browser under the installed user.
All actions must use the authenticated private broker, not a fixture backend:

- `forget-service-session` on `browser.service.<selected id>` revokes that
  selected custody and retires its native engine/profile.
- `read-service-session` on `browser.service-status` reports the selected id;
  pending revocation is `forgetting` or `forget-incomplete`. Terminal success is
  `status: forgotten`, `custody_revoked: true`,
  `engine_shutdown_confirmed: true`, `custody_backend_needed: false`,
  `local_only: false`. Local-only cleanup never qualifies.
- `list-service-sessions` on `browser.service-sessions` returns
  `{ "schema": 1, "sessions": [...] }` only after a fresh authenticated private
  List completes. Entries include `id`; an initial empty render cache is not a
  result. The receiver relaunches Browser and verifies the revoked id is absent.
- Opening the stale public `service:<id>` anchor through `tilo.app.open` must
  observe private Resolve denial. `read-service-session` reports that id with
  `status: needs-reconnect` and `requires_reconnect: true` (pending states may be
  `opening` or `waiting`).

The receiver also holds the actual native profile parent and kernel process
handles across Forget: terminal report fields alone cannot prove local profile
removal or engine exit. The final bridge receipt is emitted only after these
observations and the existing source/server/physical/namespace retirement checks.
Unavailable actions or incomplete observations fail qualification.
