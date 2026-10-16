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

Build and run in an owned disposable builder environment. For a remote
Cargo drop-in supporting `--on-builder` and `--fetch`, set `CARGO` to that client
and keep the whole journey inside its supported on-builder session:

```sh
"$CARGO" build --locked -p shell -p tilo-control --bin shell --bin tilo-control
"$CARGO" build --locked -p launcher --bin tilo-launcher \
  -p tilo-app-package-cli --bin tilo-app-package \
  -p tilo-app-cli --bin tilo-app -p install --bin tilo-install \
  -p runtime-bundle --bin tilo-runtime-bundle
"$CARGO" --on-builder --fetch native-pair-proof/journey.json run --locked \
  -p shell --example native_pair_host -- \
  --journey --out target/native-pair-proof --pipewire /private/pipewire-payload
```

`--on-builder` holds the worktree session lock through SDK package assembly,
runtime-library collection, the native windows and cleanup. Raw `podman exec`
builds do not hold that lock or record their Cargo units. Under disk pressure,
the remote artifact collector can therefore prune their newly built thin-app fingerprints and `.rlib`
files while the packager is still using them. This caused two native assembly
failures; the supported session completed the same signed voice journey.
Use `--no-fetch` for remote compile/check validation only when no local fixture
needs the returned artifacts. A locally executed native fixture needs its
libraries and generated font catalog as well as executables. The command above
executes remotely and fetches only its nonsecret receipt; `--fetch` paths are
relative to `target/`. Inside CI, ordinary Cargo is appropriate when the CI
environment already owns the complete fixture lifetime.

The disposable environment needs PipeWire utilities, WirePlumber, GStreamer
`opusenc`/`opusdec`, and a matching server payload under `--pipewire`: `usr/bin/pipewire`,
`usr/share/pipewire` and `usr/lib64/gstreamer-1.0/libgstpipewire.so`. The fixture
starts its own socket and session manager per peer; it never connects to the
operator's audio server. SDK package assembly is sequential and uses four jobs.
The output directory must be new. When running remotely, retrieve only
`journey.json`, screenshots and the bounded `*-capture-observation.json`
from the owned task directory. Never transfer `bootstrap.json` or private identity directories into evidence.
Each throwaway profile uses the existing sealed headless secret store at its
own private `TILO_SECRETS_DIR`. MLS remains mandatory; there is no plaintext
fallback when a headless environment has no Secret Service session.

## Observed production boundaries

Initial runs on 2026-10-06, based on `5fc0df4139`, admitted both independent native
peers normally and verified both private generated-source graphs, but did not
present Chat. The Applications Open invocation reports success when queued;
the fixture separately requires a presented primary foreign frame, so this cannot
be mistaken for an opened app.

1. With ordinary empty room policy, app launch preflight times out waiting for
   governor-authorized app authorship. Native membership execution refuses an
   externally governed room, and the relay alone runs no app-serving plane.
2. Delegating Chat authorship alone got as far as the ordinary signed Chat process,
   with Landlock fully enforced. The broker then refused bootstrap with
   `application creator lacks exact current governor signing custody`. An app
   authorship grant does not delegate ownership of the canonical record.

The private governing host now serves the compiled Chat application, grants its
authorship through the existing typed membership command and prepares its primary
record through the trusted application factory with actual governor custody.
Participants receive ordinary invitations and never receive the governor's key.
Terminal audit forwarding uses an authenticated, bounded room command: exact
current admission, workspace read permission and the caller's own person identity
are checked before the governor commits and publishes. The native broker also
waits to observe that exact receipt in admitted signed state.

The repaired journey has presented both ordinary signed Chat applications and
converged a newly created channel over the encrypted collaboration path. Resource
comparison uses the logical resource identity while invocation retains each
desktop's own bound component handle. Shell follow uses the certified live
device-session handle, rather than the person's identity key. Neither correction
changes production access checks. The combined journey is still unqualified.

The live roster exposed another boundary: native membership does not populate
the legacy identity-hello cache. Native app presence now checks the exact room's
current admitted read authority and the authenticated publisher's session handle
instead. The relay stamps co-edit presence attribution from the authenticated
device connection before forwarding; a caller-supplied author is never used as
native authentication evidence.

On candidate `b3ff06c7b6`, Alice's actual tab arrival reaches Bob after this
repair, but the roster still classifies her as invited: its arrival set counts
desktop pointers and legacy document participation, not room-scoped app
lifecycle presence. The governed host also has no handler for the existing
`ClaimNamespace` desktop command; its namespace request reports `Offline`,
and Alice's tab is absent from Bob's admitted topology. Both boundaries must
be completed before shell follow can qualify. No roster flag or missing-tab
assertion is bypassed by this reproducer.

`--voice-proof` runs the same certified desktops, normal packages and private
audio graphs through voice, Leave, fresh rejoin consent and removal without
running context follow. Its receipt is explicitly `mode: native-voice-proof`
and remains `qualified: false`, even when all voice steps succeed. It provides
independent evidence and cannot qualify the combined pair.

The independent voice run reaches the same `c_lounge` call on both desktops
with `muted: true` and `capture_active: false`. Alice then receives the visible
one-shot microphone prompt and chooses Allow once. The prompt clears, but her
native session ends with `permission_denied` before active capture is observed.
The bounded capture observation and failure still are retained separately from
private profile material. Opposite-frequency audio, Leave, fresh rejoin consent
and removal have not executed in that run. Subsequent diagnostic runs
present both apps but can terminate before capture with an ended, empty call
report and no issue. `call-status` explicitly returns its preceding snapshot
while requesting an asynchronous refresh; the reproducer waits for the first
service reply but refuses a later terminal state. No capture fence diagnosis
has been established by those later runs.

`journey.json` remains `qualified: false` until every step completes: presented
native Chat on both peers; opt-in context navigation; Stop and explicit follow
release; muted joining; visible fresh capture consent; decoded opposite-frequency
audio on both outputs; Leave removing audio streams; fresh rejoin consent; and
host removal closing the removed peer's streams. Follow, voice, Stop, Leave,
rejoin and revocation remain acceptance requirements until a complete successful
receipt exists. Physical Yoga/Pi journeys also remain unqualified.

The two existing lower-level native audio proofs now fail when the Opus encoder
is unavailable instead of printing SKIP and passing. They still use test media
identity/authority and must not be described as governed native Chat acceptance.
