# Native Chat resource, browser recovery and call acceptance

The existing `mounted-resource` fixture now runs against the actual Chat browser
bundle produced by `tilo-app build apps/chat --platform web`. The CLI recognizes
the SDK macro's existing application definition; no SDK source is changed.

Build `tilo-agent-participant`'s `mounted-resource` example with the configured
remote Cargo runner. Run its resulting executable with
`target/tilo-app/web/chat/bundle`, adding `--disconnect-retry` for recovery.
Configure `CHROME` for the owned fixture browser and run as an unprivileged user.
The qualified runs used Chromium's sandbox and an owned descendant supervisor.
They use only throwaway identities, profile storage, loopback HTTPS hosts,
fixture certificate exceptions and browser clipboard permission. No live account,
physical microphone, public deployment or persistent grant is involved.

The default journey retains the ordinary owner-issued Viewer invitation. It
opens the typed Chat resource through the production certified peer and resource
page, exercises visible sharing/Stop without video, observes Copy in the real
browser clipboard and Open at the exact destination, refuses an unadmitted
account without enrollment, closes the app and custody on Leave, and refuses
reopening after the workspace owner removes the reader.

The optional recovery journey receives an ordinary temporary Editor invitation.
Chromium blocks only the fixture relay origin and the existing reconnect control
closes its prior carrier. The production health snapshot must report an
unauthenticated disconnected transport. A native post is acknowledged by the
host while the browser cannot reach it. One browser post then completes locally,
its profile is flushed, and the actual peer must expose an unacknowledged
outgoing operation while still disconnected. Restoring that origin and invoking
`retry-connection` must authenticate the same certified device and drain the
outbox. Public committed reads and the native replica must contain each original
post exactly once. A genuine frame must be presented after those changes. The
original browser post is never invoked again.

Both final journeys passed against the compiled Chat bundle. Their receipts are
written separately under `target/mounted-resource/` and
`target/mounted-resource-recovery/`; the existing SDK web lane now runs and
uploads both. Missing tools, frames, acknowledgements or documents fail the
journey. The baseline deadline and existing assertions remain intact.

The optional `--voice` journey mounts that same compiled Chat in two isolated,
certified browser profiles. The host prepares the registered native primary
record and authors its existing graph through `Rooms::author_application`.
Only the exact current governor can use this host-local API; ordinary incoming
member snapshots cannot become governor records. The retained resource view
and voice admission therefore resolve the same governor-authored document.

Both profiles join the actual call muted without requesting a microphone.
Explicit Unmute runs through the public app action and the real semantic
microphone control. Chromium's synthetic devices supply production Opus audio;
each browser must report more than five decoded remote levels above RMS 0.015.
Mute stops only local capture. Leave closes tracks, audio contexts and workers,
and rejoin requires fresh Unmute. Removing one workspace member stops that
person's capture while the other participant continues. Explicit authority loss on the
authenticated media carrier closes that person's audio even if the separate
signalling end event is lost or delayed. The carrier's revocation byte or
reserved revocation close code establishes that loss; ordinary network closure
pauses capture and retains the admitted reconnect path. A late revocation of a
replaced carrier cannot end the newly admitted grant. The receipt and actual
Chat frames are saved under `target/mounted-resource-voice/`. Each run also
writes a `progress.json` receipt with a unique run id and elapsed checkpoints.
The revocation receipt distinguishes waiting for the owner's acknowledgement,
its committed generation and duration, stopping capture, closing audio resources,
and the surviving person's capture and Leave. The unchanged 130-second global
deadline writes `failure.json` before closing browsers; a successful `journey.json`
and a failure receipt must be matched by run id. CI keeps mounted artifacts under
`app-sdk-mounted-resource-<attempt>` so a later attempt cannot overwrite the
failed attempt's evidence. The SDK web lane
runs this journey as an unprivileged user with Chromium's sandbox enabled and
an owned descendant supervisor; missing prerequisites fail.

The `--encrypted-restart` journey uses another person's Editor certificate to
publish the native primary on its canonical wire route. It asserts that the
host retained that writer's ciphertext on the exact document channel, closes
the publisher and restarts the temporary host before starting the compiled
Viewer. The Viewer must materialize the original message and selected range,
share/Stop without video, Copy/Open, Leave and refuse access after owner removal.
Its receipt and frame are written to `target/mounted-resource-encrypted/`.

The focused native repro verifies the same reference succeeds before restart
and after it. Previously the cold read failed with `source channel keys
unavailable`: discovery checked only already-open stewards before a document
subscription could reopen one. Resource reads now recover the existing sealed
channel through the normal steward crash-recovery path, under the existing
current-caller read guard. Missing groups are refused without initialization;
wrong vaults/devices and corrupt state are refused. No membership is added by
this recovery, and native plaintext replay still requires the exact governor.

The encrypted fixture also uses the writer's actual person id in the shared
Chat record and checks the compiled public message read retains it separately
from the viewing person. Its `identity-boundary.json` saves only public fixture
person labels and semantics. Previously an unbound creator's fallback name was
persisted as `You`, and another reader saw that word as the message author.
Unknown creators now receive a neutral, short identity label. Legacy saved
`You` names remain untouched and are displayed with a short identity tag in
message headers, mentions and public person resources; the current reader still
sees `You` for their own messages. This does not establish Contacts lookup or
verify a person's chosen display name from the content of a message.

This qualifies local governed certified-peer resource navigation and actual
browser disconnect/retry. It does not qualify a full browser-profile restart
with queued edits, physical devices/microphones, cross-app Follow,
or the user's public demo. The audited `tilo.coffee` candidate is unconfirmed;
its resource entry returned 403. That response was not bypassed, expired AWS
credentials were not renewed, and no deployment or access change was made.
