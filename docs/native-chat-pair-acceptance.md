# Native Chat pair acceptance

The combined two-native-desktop journey is **not qualified**. Browser/native
resource acceptance and low-level native Opus tests do not qualify native/native
Chat voice, following, Stop, Leave or revocation.

## Reproducer

`scripts/headless-examples/native-pair-journey.py` drives two independent native
headless compositors with different certified people, ordinary signed thin Chat
packages, one private certified governor and separate PipeWire graphs. Capture
consent uses the visible prompt's actual one-shot control. Only generated 700 Hz
and 1100 Hz sources and null outputs are allowed; unexpected audio devices fail.
No hardware microphone, video capture or external account is used. Throwaway
identities, invitations, publisher pins and processes are removed on exit.
Forked private D-Bus helpers are stopped through pidfds only after matching both
this run's exact HOME and runtime directory. `--cleanup-proof` runs a separate,
short real-daemon check: the owned bus exits and an adjacent private bus survives.
Its receipt remains `qualified: false`, with `mode: cleanup-proof`, because it
proves cleanup rather than the native pair journey.

Build and run through rcargo in an owned disposable builder environment:

```sh
rcargo build --locked -p shell -p tilo-control --bin shell --bin tilo-control
rcargo build --locked -p launcher --bin tilo-launcher \
  -p tilo-app-package-cli --bin tilo-app-package \
  -p tilo-app-cli --bin tilo-app -p install --bin tilo-install \
  -p runtime-bundle --bin tilo-runtime-bundle
rcargo --on-builder --fetch native-pair-proof run --locked \
  -p shell --example native_pair_host -- \
  --journey --out target/native-pair-proof --pipewire /private/pipewire-payload
```

The disposable environment needs PipeWire utilities, WirePlumber, GStreamer
`opusenc`/`opusdec`, and a matching server payload under `--pipewire`: `usr/bin/pipewire`,
`usr/share/pipewire` and `usr/lib64/gstreamer-1.0/libgstpipewire.so`. The fixture
starts its own socket and session manager per peer; it never connects to the
operator's audio server. SDK package assembly is sequential and uses four jobs.
The output directory must be new. Failed rcargo runs do not automatically fetch
artifacts; read the owned remote task directory for `journey.json` and screenshots.
Never transfer `bootstrap.json` or private identity directories into evidence.

## Observed production boundaries

Runs on 2026-10-06, based on `5fc0df4139`, admitted both independent native peers
normally and verified both private generated-source graphs. Neither run presented
Chat. The original Applications Open invocation reports success when queued;
the fixture separately requires a presented primary foreign frame, so this cannot
be mistaken for an opened app.

1. With ordinary empty room policy, app launch preflight times out waiting for
   governor-authorized app authorship. Native membership execution refuses an
   externally governed room, and the relay alone runs no app-serving plane.
2. A temporary experiment delegated Chat authorship through the real governor's
   typed membership command, committed and published before joining. This got as
   far as the ordinary signed Chat process, with Landlock fully enforced. The
   broker then refused bootstrap with `application creator lacks exact current
   governor signing custody`. The experiment was removed; copying governor keys
   into a participant or weakening this custody check is not a valid fix.

The governing owner must actually serve the typed app-authoring/creation path,
with its durable receipt and publication, before this fixture can prove the full
journey. A native interactive owner has a room-scoped relay primitive, but the
shell does not currently start that primitive as a complete voice host. This
fixture does not substitute legacy direct invitations or fake media authority.

`journey.json` remains `qualified: false` until every step completes: presented
native Chat on both peers; opt-in context navigation; Stop and explicit follow
release; muted joining; visible fresh capture consent; decoded opposite-frequency
audio on both outputs; Leave removing audio streams; fresh rejoin consent; and
host removal closing the removed peer's streams. The steps after presentation
have **not executed** in this fixture. They remain acceptance requirements, not
a claim about current behavior. Physical Yoga/Pi journeys also remain unqualified.

The two existing lower-level native audio proofs now fail when the Opus encoder
is unavailable instead of printing SKIP and passing. They still use test media
identity/authority and must not be described as governed native Chat acceptance.
