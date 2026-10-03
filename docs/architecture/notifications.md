# Notification trust and delivery

The compositor is the freedesktop notification server and the only owner of
delivery policy, history, focus suppression, sounds and shell badges. Settings
edits the shared policy; it does not deliver notifications itself.

Retired and focus-suppressed notifications remain in a bounded, session-local
history exposed through Quick Settings → History, where the user can clear it.
History is intentionally not copied into Settings or persisted across sign-in:
the compositor already owns the delivery lifecycle, and avoiding a second
on-disk message archive reduces both ambiguity and sensitive-data retention.

The freedesktop `app_name`, summary, body and custom hints are sender data. For
package attribution, the service asks the session bus for the caller PID, walks
the bounded `/proc` parent chain, and accepts an application identity only when
it reaches a still-live launcher PID registered with the same kernel start
time. Direct host callers remain unattributed. Toast labels, per-app policy and
badge state therefore never use `app_name` as authority.

Sounds use the standard freedesktop sound theme, resolved in Tilo and played
with PipeWire's `pw-play`. Work is coalesced on a bounded session thread and a
wedged helper is killed after five seconds; the compositor render loop never
forks or waits. Settings reports the detected backend.

`x-tilo-badge-count` is a bounded extension because the freedesktop notification
protocol has no unread-count field. It is accepted only with verified package
attribution. The top bar consumes the aggregate now; per-app icon badges remain
unclaimed until a dock or launcher consumes the same map.

Priority contacts are sourced from favourites in Contacts. Only a
launcher-verified `so.tilo.contacts` caller may replace the shell-owned
projection. Endpoints are normalized and stored as salted BLAKE3 digests in a
private, bounded, atomically replaced record; raw endpoints are not persisted
there. A notification pierces scheduled focus only when its verified app has an
explicit user grant and its bounded contact handle matches that registry.

The authorized communication app still asserts which sender produced a
message. It is therefore part of the trusted path for that assertion; this
guarantee remains provisional until mail/chat transport itself is OS-owned.
Unknown callers, stale PIDs, unapproved apps and unmatched handles never gain
priority.
