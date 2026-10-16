# Native Chat voice qualification

Chat joins a shared-space call as a muted listener. Capture begins only after
an explicit Unmute and the current visible one-shot microphone consent. Mute,
Leave, membership revocation and stale device identity retire capture promptly.
Rejoining the same channel starts muted and needs fresh consent.

The voice lifecycle changes are independent of Follow topology and the SDK
component-construction handshake. They preserve current certified identity,
room membership, resource access, exact device binding and revocation checks.
A metadata gap retains only the same currently observed default-output
identity; it cannot keep a removed, replaced or expired device alive.

## Production-path evidence

The disposable remote-builder run `native-voice-independent-3` used two ordinary
signed native Chat packages, distinct certified identities and private PipeWire
graphs containing only generated sources and outputs. Both native windows
presented before exercising the public actions. Both peers decoded the other
peer's generated tone before and after Leave and rejoin. Explicit Mute removed
capture while retaining playback; Unmute required fresh visible consent. Both
Leave operations removed capture and playback. Rejoin exposed muted listening
with inactive capture, and governor removal closed the revoked peer's streams.
Cleanup passed. Five complete recordings each contained 96,000 samples; their
expected-tone energy fraction exceeded 91%.

The first two independent attempts failed before calls because the disposable
container lacked the private fixture payload and its audio tools. Their cleanup
passed. The third run completed after those fixture dependencies were supplied.
No physical microphone, user desktop, screen sharing or public listener was used.

Run the existing `native_pair_host` example with `--journey --voice-proof`,
`--out target/<receipt-dir>` and `--pipewire <private-fixture-prefix>` through the
configured remote Cargo runner. Build the normal shell, launcher, control,
package and runtime tools first, as described in
[native pair acceptance](native-chat-pair-acceptance.md). With the same configured `CARGO` drop-in supporting remote execution, run:

```sh
"$CARGO" --on-builder --fetch native-voice-proof/journey.json run --locked \
  -p shell --example native_pair_host -- \
  --journey --voice-proof --out target/native-voice-proof \
  --pipewire /private/pipewire-payload
```

The private audio payload must exist inside that builder container. Keep the
whole fixture in the supported session so artifact GC cannot remove its
thin-app libraries during assembly; do not launch the packager through raw
container execution. The output directory must be new. Missing dependencies or
missing evidence fail the journey. The receipt has `mode: native-voice-proof`, no error and no cleanup
error. Its `qualified: false` field deliberately reserves full-pair qualification
for the combined Follow and voice journey.

The independent branch also passed 26 focused shell media tests, 13 Chat local
voice-control tests, 33 GStreamer voice tests, and the required shell integration
compilation. Script linting and staged repository checks passed. The exact Vulkan presentation
regression also passed with the current main capture-ordering fix, preserving
both commit and pixel-sequence guards. The old test failed twice in the held
Chat CI run and passed once at the untouched base; no deterministic base failure
was reproduced.

## Limits

This evidence qualifies the generated-audio native lifecycle. It does not
qualify physical devices, a browser voice client, disconnect/retry behavior or
Follow presentation. The strict followed-tab assertion remains in the combined
fixture; an unavailable peer-window frame cannot count as successful Follow.
