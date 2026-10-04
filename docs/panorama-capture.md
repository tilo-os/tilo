# Shared-host panorama capture

The browser runtime can compose an explicit allowlist of **single-app mounts**
into a bounded, read-only canvas. The host copies each source at a completed
presentation, including its actual record ID, logical/backing sizes, GPU
generation and frame number. A camera transforms source pixels and that
presentation's observed pointers together. The returned receipt describes every
included source. This is a **shared-host composition of retained mounts**, not a
capture of the native desktop or evidence of simultaneous model execution.

Implementation was based on main `75b68f1c31`; the earlier launch-plan inspection
was of `2cb141d7a1`. Presence lifetime/focus fixes are in PR #2383,
`4da5adb602`. Neither source inspection qualifies an 80-agent shoot.

## Public host interface

`mountDesktop()` returns `captureSource`, an opaque handle valid only for that
mount. Import `createPanorama` from `web/host.mjs` (or `web/panorama.mjs`).
Use canonical app IDs in the mounted app options; capture does not resolve
aliases such as `writer` into a different presence app ID. Supply only the
isolated synthetic/demo mounts intended for the take, and a separate output
canvas (an app backing canvas is refused):

```js
import {createPanorama} from './host.mjs';

// mounts are already admitted, mounted, and bound to their actual records.
// Do not open apps, request credentials, or spawn agents as part of capture.
const observer = createPanorama({
  canvas: outputCanvas, width: 1920, height: 1080,
  sources: mounts.map((mount, p) => ({
    source: mount.captureSource,
    rect: [520 + 1080 * Math.floor(p / 4), 180 + 660 * (p % 4), 960, 540],
  })),
});
const camera = {cx: 3000, cy: 1440, scale: 0.32};
// After the allowed sources have presented new frames:
const {blob, receipt} = await observer.png(camera, {
  maxAgeMs: 1000, maxSkewMs: 1000,
});
// Save the PNG and receipt together. Errors are capture gaps, never success.
observer.close();
```

`render(camera, bounds)` synchronously paints and returns the receipt without
encoding. `png` allows only one encode in flight; callers apply backpressure.
`setVisible(false)` blanks the output and releases observation/copies.
`close()` is idempotent and releases only observer resources. Neither operation
ends agent sessions, shuts down mounts or changes their visibility/focus.
There are no animation timers, model calls, input events, or presence writes.

A newly observed or newly revealed source must **present again** before it can
be used. Offscreen panes release their copied backing surface; revealing one
without a new presentation fails with a gap. Prepare the director's next camera
pose before releasing the corresponding work, and retry capture only after a
real presentation. Do not synthesize pointer movement or force document edits
just to satisfy capture. A paused/idle source may require a deliberate larger
`maxAgeMs`; retain its original timestamp rather than claiming a fresh frame.
Frames from different mounts are not asserted to be simultaneous: the caller
sets `maxSkewMs`, and each source timestamp is retained.

## Identity, coordinates and privacy

Sources must share the same host audience tuple `(profile, device, room)`.
The allowlist does not enumerate other mounts, browser tabs, windows, files or
the desktop. It adds no credentials or grants. Admission/cryptographic audience
checks remain the existing peer/live transport's responsibility. The observer
checks that a pointer's publisher remains a member in the same membership
lifetime and its focus exactly matches the source's **app and actual record**.
Display labels and colors are presentation metadata, not identity.

Only finite, in-content LIVE_V1 pointers are drawn. They are the existing live
receiver's sampled positions (which can interpolate motion); they are not proof
of physical mouse input. Missing pointer events remain missing. `presenceAvailable: false` in a pane
receipt explicitly reports the absence of a live layer and must block a take
that promises agent presence. MCP agent-run
attribution alone creates no pointer. No released/working/completed counter is
inferred. Carets/selections already painted by the app are included in its
pixels; this observer does not reconstruct selection anchors or animate carets.

Source coordinates are logical pixels. The source copy uses its observed
backing size; the transform maps logical coordinates to the pane's world rect,
then applies `screen = outputCenter + scale * (world - cameraCenter)`.
Every pixel, cursor and label is clipped to that pane and the output. There is
no clamping an outside pointer onto an edge and no interpolation across focus
changes. Capture does not change source scrolling, zoom or focus.

Disposed/replaced/crashed mounts, changed dimensions without presentation,
missing/old/conflicting generations, wrong audiences, unknown handles,
overlapping panes, stale frames and excessive source skew are errors. A failed
render blanks the output. Reconnected publishers cannot reuse old cached
coordinates. Pointers expire after the existing 10-second silence threshold.
Source snapshots preserve their presentation timestamp; the observer timestamp
and sequence count observations, not newly executed app actions.

Limits: at most 256 observed members per source and 20 non-overlapping panes; each source at most 2,097,152 backing
pixels and 4096 pixels on either axis; output at most 3840×2160 pixels, each
axis at most 4096; camera scale 0.05–8. No world-sized GPU target is allocated.
Worst-case source copies are 160 MiB, output about 32 MiB, plus browser/GPU and
one pending PNG encoding's memory. These are capacity bounds, not a measured
performance or frame-rate claim. Unsupported sources fail rather than downscale
silently. Use a source backing resolution such as 1920×1080 or 960×540.

## Python director integration

Use an already authorized isolated browser connection. The application harness
owns `window.takePanorama = observer`; never attach this code to the user's
ordinary browser session. The example accepts a Playwright-compatible `page`
already provided by the caller and installs nothing. It encodes one frame at a
time, saves its receipt and surfaces every capture error to the director:

```python
import base64
import json
from pathlib import Path

async def capture_frame(page, camera, destination):
    result = await page.evaluate('''async camera => {
      const {blob, receipt} = await window.takePanorama.png(camera);
      const data = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result.split(',')[1]);
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(blob);
      });
      return {data, receipt};
    }''', camera)
    path = Path(destination)
    path.write_bytes(base64.b64decode(result['data'], validate=True))
    path.with_suffix('.json').write_text(json.dumps(result['receipt'], indent=2))
    return result['receipt']

# Pure camera interpolation; does not release agents or claim their progress.
def camera_between(a, b, t):
    u = min(1.0, max(0.0, (t-a['t'])/(b['t']-a['t'])))
    s = u*u*u*(u*(u*6-15)+10)
    return {k: a[k] + (b[k]-a[k])*s for k in ('cx', 'cy', 'scale')}
```

The production manifest's `[t,cx,cy,scale]` keys are
`[0,768,432,1.25]`, `[2.4,768,432,1.25]`, `[4,1350,650,1]`,
`[6,1950,920,.72]`, `[8,2450,1220,.49]`, `[10.6,3000,1440,.32]`,
`[13.2,3000,1440,.32]`. Keep real capture gaps and source timestamps. Editing
chronology, compositing, or replay must retain the plan's disclosure.

## Qualification and remaining rollout

Run `node --test crates/browser/tilo-browser-runtime/web/panorama.test.mjs` and
`host-input.test.mjs` for production-path lifecycle/geometry assertions,
including the explicitly synthetic 80-presence fixture. Existing peer tests
cover sealed-frame refusal, endpoint grants and reconnect behavior. Run
`node crates/browser/tilo-browser-runtime/scripts/panorama-pixels.mjs` for actual
Canvas2D/WebGPU source copying, clipping, pointer alignment and PNG decode in a
fresh headless Chrome profile. It uses only synthetic geometry; no Tilo app
content, private desktop or real model sessions. Chrome must already exist.

Before filming: qualify actual SDK app mounts at the target resolution, exact
record and authenticated agent identities, browser/GPU presentation fidelity,
source freshness/skew and task readback. The current API requires single-app
mounts; a whole-desktop mount and DOM-overlaid legacy/video content are not
supported. Native Mac/Wayland retained-surface capture and semantic MCP session
→ authenticated live presence are **not implemented by this increment**.
Those need separate adapters with equivalent admission and coherence evidence.
The existing all-nine performance gates remain independent prerequisites; no
new benchmark, model/API call, credential grant or private capture is authorized
by this feature. Synthetic success establishes neither 80 real agents nor 80
concurrent tasks, runtime throughput, unique presented fps, or shoot readiness.
