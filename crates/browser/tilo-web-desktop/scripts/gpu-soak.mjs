// Soak a desktop in headless Chromium and check that its GPU and wasm
// resources stay bounded: live WebGPU buffers and textures, wasm memory, the
// canvas backing store, and the largest compute dispatch. A per-frame buffer
// or texture leak, unbounded wasm growth, or a canvas past the renderer's caps
// is what exhausts a browser's GPU process — on Safari that took the whole Mac
// down. Never point this at Safari or Playwright's WebKit.
//
// usage: node gpu-soak.mjs URL [WINDOWS] [MINUTES]
//   URL      a desktop page: build.sh's desktop output (the page root …/, where
//            windows are opened through globalThis.tiloMount) or a console
//            page that mounts host.mjs (its existing room is soaked)
//   WINDOWS  windows to open on the desktop page (default 60)
//   MINUTES  soak time after the windows are open (default 3)
//   VIEWPORT logical page size (default 3008x1692: a 6K Retina display) at
//            DPR (default 2), emulated. Emulation does not scale the canvas's
//            device-pixel box; NOEMU=1 keeps a Chromium started with
//            --force-device-scale-factor=2 --window-size=3008,1692 instead.
//   Chromium started with --headless=new --enable-unsafe-webgpu
//            --remote-debugging-port=${CDP_PORT:-9333}
// Prints one JSON line per sample and a summary; exits non-zero when a
// bound is broken.
const [url, windowsArg, minutesArg] = process.argv.slice(2);
if (!url) { console.error('usage: node gpu-soak.mjs URL [WINDOWS] [MINUTES]'); process.exit(2); }
const windows = Number(windowsArg ?? 60);
const minutes = Number(minutesArg ?? 3);
const port = process.env.CDP_PORT ?? '9333';
const SAMPLE_MS = 10_000;
// The runtime's caps (tilo-browser-runtime/src/viewport.rs).
const MAX_SURFACE_SIDE = 4096, MAX_SURFACE_PIXELS = 3840 * 2160;

const instrument = `(() => {
  const s = globalThis.__gpuSoak = {
    buffers: {live: 0, liveBytes: 0, created: 0, largest: 0},
    textures: {live: 0, liveBytes: 0, created: 0, largest: 0},
    dispatch: {count: 0, indirect: 0, largestWorkgroups: 0, largestDims: [0, 0, 0]},
    submits: 0, queueMs: [], memories: [], adapters: 0,
  };
  const track = (kind, object, bytes) => {
    const k = s[kind]; k.live++; k.liveBytes += bytes; k.created++; k.largest = Math.max(k.largest, bytes);
    let gone = false;
    const release = () => { if (gone) return; gone = true; k.live--; k.liveBytes -= bytes; };
    const destroy = object.destroy;
    object.destroy = function () { release(); return destroy.call(this); };
    registry.register(object, release);
  };
  const registry = new FinalizationRegistry(release => release());
  const requestAdapter = GPU.prototype.requestAdapter;
  GPU.prototype.requestAdapter = function (...args) { s.adapters++; return requestAdapter.apply(this, args); };
  const createBuffer = GPUDevice.prototype.createBuffer;
  GPUDevice.prototype.createBuffer = function (d) { const b = createBuffer.call(this, d); track('buffers', b, Number(d.size)); return b; };
  const createTexture = GPUDevice.prototype.createTexture;
  GPUDevice.prototype.createTexture = function (d) {
    const t = createTexture.call(this, d);
    const size = Array.isArray(d.size) ? d.size : [d.size.width, d.size.height ?? 1, d.size.depthOrArrayLayers ?? 1];
    track('textures', t, size.reduce((a, b) => a * (b ?? 1), 1) * 4 * (d.mipLevelCount ?? 1));
    return t;
  };
  const dispatch = GPUComputePassEncoder.prototype.dispatchWorkgroups;
  GPUComputePassEncoder.prototype.dispatchWorkgroups = function (x, y = 1, z = 1) {
    s.dispatch.count++;
    const n = x * y * z;
    if (n > s.dispatch.largestWorkgroups) { s.dispatch.largestWorkgroups = n; s.dispatch.largestDims = [x, y, z]; }
    return dispatch.call(this, x, y, z);
  };
  const indirect = GPUComputePassEncoder.prototype.dispatchWorkgroupsIndirect;
  GPUComputePassEncoder.prototype.dispatchWorkgroupsIndirect = function (...args) { s.dispatch.indirect++; return indirect.apply(this, args); };
  const submit = GPUQueue.prototype.submit;
  GPUQueue.prototype.submit = function (buffers) {
    s.submits++;
    const start = performance.now();
    const result = submit.call(this, buffers);
    this.onSubmittedWorkDone().then(() => { s.queueMs.push(performance.now() - start); if (s.queueMs.length > 2000) s.queueMs.splice(0, 1000); });
    return result;
  };
  for (const name of ['instantiate', 'instantiateStreaming']) {
    const original = WebAssembly[name];
    WebAssembly[name] = async function (...args) {
      const result = await original.apply(this, args);
      const memory = (result.instance ?? result).exports?.memory;
      if (memory instanceof WebAssembly.Memory) s.memories.push(memory);
      return result;
    };
  }
})()`;

const sample = `(() => {
  const s = globalThis.__gpuSoak;
  const host = document.querySelector('.tilo-host');
  const q = [...s.queueMs].sort((a, b) => a - b);
  const pct = p => q.length ? Math.round(q[Math.min(q.length - 1, Math.floor(p * q.length))] * 10) / 10 : null;
  const canvas = host?.querySelector('canvas');
  return {
    phase: host?.dataset.phase ?? null, frames: Number(host?.dataset.frames ?? 0),
    alert: document.querySelector('[role=alert]')?.textContent ?? null,
    buffers: {...s.buffers}, textures: {...s.textures}, dispatch: {...s.dispatch}, submits: s.submits, adapters: s.adapters,
    queueMs: {p50: pct(0.5), p95: pct(0.95), max: pct(0.999)},
    wasmBytes: s.memories.reduce((a, m) => a + m.buffer.byteLength, 0),
    jsHeap: performance.memory?.usedJSHeapSize ?? null,
    canvas: canvas ? {width: canvas.width, height: canvas.height, cssWidth: canvas.clientWidth, cssHeight: canvas.clientHeight, dpr: devicePixelRatio} : null,
    windows: globalThis.tiloMount?.navigation?.getSnapshot().entries?.length ?? null,
  };
})()`;

const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once: true}));
let id = 0;
const pending = new Map();
const listeners = [];
ws.addEventListener('message', ({data}) => {
  const msg = JSON.parse(data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  else for (const l of listeners) l(msg);
});
const send = (method, params = {}, sessionId) => new Promise(resolve => {
  const n = ++id; pending.set(n, resolve);
  ws.send(JSON.stringify({id: n, method, params, sessionId}));
});
const sleep = ms => new Promise(r => setTimeout(r, ms));
const {result: {targetId}} = await send('Target.createTarget', {url: 'about:blank'});
const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
const evaluate = async expression => {
  const r = await send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}, sessionId);
  if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description ?? r.result.exceptionDetails.text);
  return r.result?.result?.value;
};
const log = [];
listeners.push(msg => {
  if (msg.sessionId !== sessionId) return;
  if (msg.method === 'Runtime.exceptionThrown') log.push('EXC ' + (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text).slice(0, 300));
  if (msg.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(msg.params.type)) log.push(msg.params.type + ' ' + msg.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 300));
});
await send('Runtime.enable', {}, sessionId);
await send('Page.enable', {}, sessionId);
await send('Page.addScriptToEvaluateOnNewDocument', {source: instrument}, sessionId);
if (process.env.USER_AGENT) await send('Emulation.setUserAgentOverride', {userAgent: process.env.USER_AGENT}, sessionId);
const [width, height] = (process.env.VIEWPORT ?? '3008x1692').split('x').map(Number);
if (!process.env.NOEMU) await send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: Number(process.env.DPR ?? 2), mobile: false}, sessionId);
await send('Page.navigate', {url}, sessionId);

let s;
for (let i = 0; i < 120; i++) {
  await sleep(1000);
  s = await evaluate(sample);
  if (s.phase === 'failed' || s.alert || (s.frames > 0 && (s.windows !== null || !new URL(url).pathname.endsWith('/')))) break;
}
const samples = [];
const record = label => evaluate(sample).then(v => { samples.push({label, t: Date.now(), ...v}); console.log(JSON.stringify({label, ...v, queueMs: v.queueMs})); return v; });
await record('mounted');
if (process.env.USER_AGENT) {
  console.log(JSON.stringify({summary: 'user-agent-guard', alert: s.alert, adapters: s.adapters, phase: s.phase, log: log.slice(0, 6)}));
  await send('Target.closeTarget', {targetId}); ws.close();
  process.exit(s.alert && s.adapters === 0 ? 0 : 1);
}
if (s.windows !== null) {
  const apps = await evaluate(`fetch('apps.json').then(r => r.json()).then(j => j.applications.map(a => a.id))`);
  for (let i = 0; i < windows; i++) {
    const app = apps[i % apps.length];
    await evaluate(`(() => { const n = globalThis.tiloMount.navigation; return n.dispatch({kind: 'launch', generation: n.getSnapshot().generation, app: ${JSON.stringify(app)}}); })()`)
      .catch(error => log.push(`launch ${app}: ${error.message}`));
    await sleep(300);
    if (i % 20 === 19) await record(`opened ${i + 1}`);
  }
}
await sleep(10_000);
const warm = await record('warm');
const deadline = Date.now() + minutes * 60_000;
let step = 0;
while (Date.now() < deadline) {
  // Keep the renderer drawing: select the windows in turn when the page offers navigation.
  await evaluate(`(() => { const n = globalThis.tiloMount?.navigation; if (!n) return; const s = n.getSnapshot(); const e = s.entries ?? []; if (!e.length) return; return n.dispatch({kind: 'select', generation: s.generation, route: e[${step} % e.length].route}); })()`).catch(error => log.push(`select: ${error.message}`));
  step++;
  await sleep(SAMPLE_MS);
  await record(`soak ${step}`);
}
const last = samples.at(-1);
const peak = key => Math.max(...samples.filter(x => x.t >= warm.t || x === warm).map(x => key(x)));
const summary = {
  url, viewport: `${width}x${height}@${process.env.DPR ?? 2}`, windows: last.windows, minutes,
  canvas: last.canvas, frames: [warm.frames, last.frames],
  buffers: {warm: warm.buffers, last: last.buffers, peakLiveBytes: peak(x => x.buffers.liveBytes)},
  textures: {warm: warm.textures, last: last.textures, peakLive: peak(x => x.textures.live)},
  wasmBytes: [warm.wasmBytes, last.wasmBytes], jsHeap: [warm.jsHeap, last.jsHeap],
  dispatch: last.dispatch, queueMs: last.queueMs, submits: last.submits,
  log: log.slice(0, 12),
};
console.log(JSON.stringify({summary}));
await send('Target.closeTarget', {targetId});
ws.close();
const MiB = 1 << 20;
const problems = [];
if (last.phase !== 'presented') problems.push(`desktop is not presenting: ${last.phase} ${last.alert ?? ''}`);
if (last.frames <= warm.frames) problems.push('no frame was presented during the soak');
if (last.canvas && (Math.max(last.canvas.width, last.canvas.height) > MAX_SURFACE_SIDE || last.canvas.width * last.canvas.height > MAX_SURFACE_PIXELS)) problems.push(`canvas backing store ${last.canvas.width}x${last.canvas.height} is past the caps`);
if (last.buffers.liveBytes > Math.max(warm.buffers.liveBytes * 1.5, warm.buffers.liveBytes + 64 * MiB)) problems.push(`live GPU buffers grew from ${warm.buffers.liveBytes} to ${last.buffers.liveBytes} bytes`);
if (last.textures.live > warm.textures.live + 8) problems.push(`live GPU textures grew from ${warm.textures.live} to ${last.textures.live}`);
if (last.wasmBytes > warm.wasmBytes + 256 * MiB) problems.push(`wasm memory grew from ${warm.wasmBytes} to ${last.wasmBytes} bytes`);
if (log.some(l => l.startsWith('EXC') || /panic/i.test(l))) problems.push('the page threw or panicked');
for (const p of problems) console.error(`FAIL ${p}`);
process.exit(problems.length ? 1 : 0);
