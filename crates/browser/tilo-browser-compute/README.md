# Private browser image computation

Experimental, opt-in operation isolation. The default browser image path does not
use this crate yet. It links only the portable image codec and wasm-bindgen glue,
with a private 256 MiB maximum linear memory (4096 pages), enforced by the linked
artifact. This ceiling excludes JS buffers and browser RSS. Metadata admission
is 16 MiB encoded, 4096 source dimension and 64 MiB source RGBA; advisory codec
allocation limits are not a strict sandbox. Allocation aborts/traps may bypass a
Rust Result: the parent handles worker errors, message errors and watchdog expiry.

The trusted host installs **one** `createComputeDomains()` per page. Bind a
canonical live owner once with `bindOwner({owner, lifetime, domain, lane, signal,
authorize})`. A duplicate live owner/lifetime binding is refused, including a
different domain/lane. IDs are bounded strings for correlation, not authority.
The host chooses lane (`foreground` or `background`), current authorization and a
fresh source identity tied to exact admitted bytes/ticket, never a URL alone.
`authorize()` must return true before dispatch and before delivery; closure wins
reentry. It does not replace the Rust image ticket's final publication check.

```js
const images = domains.bindOwner({owner, lifetime, domain: 'images', lane,
  signal: ownerSignal, authorize: () => hostStillAuthorizes(owner)});
await images.decode({bytes, source: ticketId, maxDimension: 2048}, async result => {
  // Recheck the exact host ticket, owner and dimensions before publication.
  await consumeDecodedPixels(result);
});
```

The consumer's returned promise owns result custody. It must stop retaining the
pixels when it settles. The job/byte reservation stays charged through the
consumer, including after owner abort; a consumer that never settles retains its
reservation. Closing rejects promptly and terminates running work. It cannot
revoke references already delivered to trusted consumer code.

Limits are two exclusive workers total, one background worker, one active lease
per live owner binding, eight accepted jobs, 32 MiB encoded and 192 MiB managed
reservations. Each job reserves twice its input plus twice the maximum requested RGBA plus
two chunks (the consumed callback and the next credited transfer can overlap).
The two full RGBA reservations cover simultaneous
worker JS output and page JS staging (WASM linear memory remains separate). Jobs, preparing copies, queued inputs and unconsumed
results count until their real custody ends. Each accepted active job gets a new
worker/WASM instance, terminated on result/failure/cancel; there are no idle
workers or recurring idle timers. Ready owners rotate, preserving FIFO per owner.
Termination is abort/restart, not pause/resume; no automatic retries occur.

Input views are read with intrinsic typed-array fields. Shared/detached buffers
are refused, cached buffers are never detached, and unrelated backing bytes are
not copied. The trusted host must keep input contents stable and attached until
`decode` settles: preparation spans tasks and cannot atomically snapshot mutable
bytes, and its end is not a public caller notification.
Source identity remains tied to that exact stable content and host ticket.
Exact input copies yield real MessageChannel tasks every <=256 KiB.
Outputs have one <=256 KiB credit at a time; dimensions, exact length, identity
and sequential offsets are checked before consumption. A full result allocation
still occurs in one callback: its duration, header callback, chunk callback,
input copy, worker decode/startup and staging are measured by qualification. These
measurements are observations, not worst-case GC or 120 Hz guarantees. JS/WASM
input/output copies exist; this is not zero-copy.

## Reproduce standalone qualification

Use the repository's required Cargo wrapper/environment and disk floor:

```sh
bash crates/browser/tilo-browser-compute/scripts/build.sh
cargo run --locked -p tilo-browser-compute --example fixtures -- target/browser-compute/fixtures
node --test crates/browser/tilo-browser-compute/web/compute-domains.test.mjs
node crates/browser/tilo-browser-compute/scripts/qualify.mjs
```

The last command requires installed Chrome and ephemeral loopback binding. It
fails instead of skipping if unavailable, closes its browser/server and emits
`target/browser-compute/qualification.json` with source hashes, actual asset
hashes, actual WASM memory/import inspection, native pixel-oracle comparisons for
PNG/JPEG/WebP, >1 second of real repeated decode, page heartbeat overlap with
worker CPU intervals, real cancellation, malformed/oversized rejection,
deliberate trap/hang termination, and runtime memory growth refusal. The separate
release compute WASM gate is an initial 1 MiB limit; dev modules have no byte
budget. Both profiles check final memory and allow only bindgen string/error
imports; dev additionally permits bindgen’s own externref-table initializer. The desktop main-module budget is unchanged.

## Integration gates

Both canonical builders (runtime/desktop shell script and app/desktop SDK CLI)
package lazy top-level `compute-*.mjs` and `pkg/compute/`. This only assembles
assets; it does not enable the image route or initialize workers. SDK compute
builds use the canonical SDK workspace's manifest, lock and profile, even when
the app runtime builds in a generated workspace. Its cache binds raw WASM,
profile, bindgen/optimizer versions and flags, worker sources and all five
required emitted files; missing or changed JS/WASM rebuilds. `build.json` keeps
separate compute byte/hash/tool/memory evidence. Immutable versions include worker
assets in the existing digest and registry archive and refuse partial bundles.

To qualify a canonical release output, use a fresh directory outside the product:

```sh
node crates/browser/tilo-browser-compute/scripts/qualify-packaged.mjs \
  target/tilo-app/web/calculator/bundle /tmp/compute-qualification
```

This copies exact worker bytes, verifies their hashes, generates the native pixel
oracle in staging and runs real Chrome there. It verifies every product file is
unchanged and records canonical hashes in the qualification receipt. Never run
`qualify.mjs` in-place on a product bundle: it writes fixture modules and replaces
`index.html`. The existing app-sdk Chrome lane qualifies actual release output
and uploads its receipt. P6 must use canonical scheduler
policy/owner custody and Rust completion tickets and prove foreground plus 100
actually mounted apps. Standalone page heartbeats do not prove those integration
gates, OS RSS bounds, 120 Hz performance or Notes end-to-end behavior. Same-origin
Worker glue is trusted code, not a hostile-JavaScript security sandbox. Image
fetch authority stays in the host; the worker only loads its static code assets.
