# Native task model dispatch and Stop

The model gateway currently authenticates an application connection, not an exact
native task run. Agent task cancellation is polled around model turns, but a
separately running transport thread or a gateway request waiting on configuration
or a retry can continue after that poll. Ending the task or dropping its response
receiver does not itself revoke the outbound provider request. Stop must not be
presented as proof that no later model HTTP request can start.

Private person conversations do not change task execution, native action
confirmation or model transport. They do not resolve this existing limitation.
Likewise, authenticated executor enrollment alone is not protected model dispatch.

## Required connection

A native managed launch needs a protected model connection bound to the actual
actor and leased run, distinct from ordinary Agent Plan/Transform connections.
Freeze the selected model/provider contract through the host's existing Settings,
account and vault path. Bind each final outbound body to an exact, single-use
durable invocation. After queue waits and retries, release the real HTTP task
under the same governing mutation turn as Stop and the applicable host fence.
A permission boolean checked before an independently scheduled POST leaves the
race open. Denied or abandoned dispatch must be incapable of issuing HTTP.

Keep ordinary Agent requests, OAuth accounts, provider protocols and explicitly
unauthenticated local endpoints compatible. New automatic native execution must
not use enrollment or read-only readiness as a substitute for this connection.

## Regression proof

Use the production launch-issued connection and an actual loopback HTTP server.
Delay native transport, complete Stop and any required host acknowledgement,
release the delay, and assert that no new native POST arrives. Repeat for Plan,
Transform, supported provider protocols, retries, replay, changed selections and
restart. Enrollment, action-confirmation and mock dispatch-counter tests alone
do not prove this transport property.
