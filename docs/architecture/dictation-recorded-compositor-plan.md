# Bounded recorded-audio compositor qualification

This is the next assembled acceptance journey for the held dictation candidate.
It requires no physical microphone, account, paid API, persistent permission,
trust-store installation, component deployment, VM swap or reboot. It is prepared,
not a claim that the current source head has passed the assembled journey.

## Target and isolation

Run on the existing remote x86_64 review builder in this task's own rcargo tree,
`/srv/rcargo/p/23a3d92b5603/w/a87aa1f3353c`, not the connected laptop's real desktop.
The connected laptop is Bazzite 44, x86_64; it currently has neither the fixed-path
speech executable nor the image catalogue/key files required by this candidate.
Do not install them there to make the fixture pass.

Use the existing hardware-disabled PipeWire/WirePlumber fixture, which previously
passed assembled capture, original-field insertion, native history, cancellation
and focus retirement (the 6 October checkpoints in `dictation.md`). Rebuild current
Shell and the maintained SDK `browser-text-input-qualification --native-fixture`
through rcargo, with no local fallback. Bind the pinned CEF qualification runtime
read-only into the disposable namespace. The direct native fixture proves the
compositor contract without first rebuilding the signed thin-app publisher path.
Qualification of the installed signed app remains separate.

Use one foreground disposable container/PID namespace and a bubblewrap child:
no external network; no host home, session sockets, physical devices or DRM;
private `/tmp`, profile, runtime, identity and authority directories; `/dev`
contains only bubblewrap's synthetic device set. Limit the outer scope to two
CPUs, 3 GiB memory and 256 processes, and the whole journey to 240 seconds.
Do not relax the worker's 1 GiB address-space cap or its resource admission.
A pressure refusal is a failure receipt, not permission to retry in a loop.

## Model and audio inputs

For the existing assembled Whisper control, use the maintained
`local-model-service` example `speech_qualify --hold-fixture` with an already
reviewed Tiny artifact. Its source verifies the exact allowed digest and size,
creates disposable signed catalogue/licence/store inputs, and removes them on
owner EOF. Bind these into the child's production paths only. The source worker
must run non-root through `/usr/bin/tilo-speech-worker`; do not introduce a
production executable override or development trust key. Use the already
verified public FLEURS `en_us-0` waveform and its benchmark receipt, played by
`pw-play` into `TiloPublicAudioSink`. Verify that the only capture target is
`TiloPublicAudioSource`, with no `alsa_` or `bluez_` nodes.

A Whistle run additionally needs the existing default-off AOT image witness,
sealed licence-accepted `.cact`, runtime and source receipts mounted in this
same namespace. The reviewed Whistle source is Cactus-Compute/whistle, revision
`b358ddadd89b7a713b5aa131f23032d3cca1b251`, model SHA-256
`b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb`.
Wasmtime is 49.0.2 at `3c8a3e79aa3188a1a12b3aebbaa097abccceac92`; the runtime
package targets x86-64-v3. Reuse and verify the existing receipts, not caller
metadata as execution authority. The previous worker/browser concurrent test
is not an assembled Whistle compositor/capture result.

## Observable journey

Use the existing `tilo-control` socket operations against the private headless
compositor. Start an empty public fixture with two fields and a selected range
in the first. Establish actual native window focus, not merely DOM focus.

1. Invoke `actions/dictation.toggle`. Require private original-field Prepare ACK,
   the trusted consent frame and a bound active text-input owner.
2. Accept only the synthetic source's one-session prompt. Require private Start
   ACK and worker Ready before the recorder appears. No audio opens beforehand.
3. Play the verified waveform. Drafts must leave both field values unchanged.
   Invoke `actions/dictation.finish`; require recorder exit before final delivery,
   exactly one original-field final ACK and unchanged successor field.
4. Invoke native Edit Undo/Redo (or the maintained fixture's ordinary Ctrl+Z and
   Ctrl+Shift+Z control journey), checking exact selected-range restoration and
   one owned undo transaction. A document CRDT undo is not native field history.
5. Start another session, cancel during playback, and require both recorder and
   worker to exit without a final edit. Start a fresh session only after teardown.
6. During fresh playback, move focus away and back; require permanent retirement,
   unchanged fields and no late final delivered to either field.

Bound every wait; fail if the subject cannot be observed. Preserve exit codes,
source/host/model/audio hashes, private ACK states and cleanup failures. Export
only public synthetic text. Stop and reap only exact owned child/process-group
identities, await native descendants before deleting profiles, close the held
model owner's input, and verify ephemeral inputs are gone. Never use process-name
kills, screenshot the user's desktop or leave a compositor running.

## Release and physical microphone approval boundary

Ordinary first-party component installation is not itself a new security grant.
Here it remains held because this session explicitly forbids deployment, component
swaps and activation on the real OS. Preparing or executing the disposable fixture
above does not cross that restriction.

Persistent Whistle release is a separate concrete action: review the exact model
licence, add model/runtime entries through the protected local-model publisher,
produce its signed catalogue under the existing release key, and install the
verified package/store receipt in a selected Tilo OS test environment. The tracked
production catalogue currently has no speech model or runtime, so generic runtime
readiness cannot authorize this action. No release key change is needed or allowed.

For a later physical test, select the exact owned Tilo test session and PipeWire
source first, then obtain one-session capture consent for that source. The code's
`DictationSurface` prompt binds consent to the session ID and does not persist a
microphone grant; an earlier canceled/finished session or an unrelated host-app
permission cannot cover the new session. Existing PipeWire access has not been
verified for a future physical source. Request that precise capture action only
when the source/session and approved model/runtime are concrete. No broad
microphone grant, auto-confirmation, default-source fallback or account permission
is part of this plan.

## Current input preflight (9 October)

The task's builder contains Shell, the SDK browser example, speech witness and
worker binaries, the verified public benchmark manifest and the new CEF host.
These existence checks do not establish current-source binary provenance.
The old `target/speech-benchmark-v2/tiny.bin` and `target/audio-fixture-tools`
server/tool staging are absent in this tree. Host PipeWire, WirePlumber and
bubblewrap exist; `pw-record` and `pw-play` do not. Restore the already verified
Tiny artifact and signature-checked audio tools only inside the disposable
fixture (or use the held Whistle witness plus the same isolated audio tools)
and rebuild the current Rust binaries before executing this plan. This is a
fixture-input prerequisite, not a microphone or persistent-access approval.
