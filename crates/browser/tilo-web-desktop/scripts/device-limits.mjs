// Open many windows on a built desktop bundle in headless Chromium, on a
// WebGPU device held to the spec's default buffer limits (128 MiB per storage
// binding), and check the desktop keeps presenting. A desktop that encodes
// every window, or a renderer that sizes a buffer past the device's limits,
// fails its device with a createBindGroup validation error instead.
//
// usage: node device-limits.mjs BASE_URL [WINDOWS]
//   BASE_URL  where build.sh's desktop output is served (serve.mjs)
//   WINDOWS   windows to open (default 36)
//   VIEWPORT  logical page size (default 1728x1117) at DPR (default 2): a
//             Retina laptop, where the renderer's buffers are largest
//   Chromium  started with --headless=new --enable-unsafe-webgpu
//             --remote-debugging-port=${CDP_PORT:-9333}
// Exits non-zero when the desktop fails, throws, stops presenting, or any
// GPU buffer is created larger than the limit.
const [base, windowsArg] = process.argv.slice(2);
const windows = Number(windowsArg ?? 36);
const port = process.env.CDP_PORT ?? '9333';
const LIMIT = 128 * 1024 * 1024;

// Runs in the page before any script: no device may exceed LIMIT, whatever
// the adapter offers, and the largest buffer created is recorded.
const clamp = `(() => {
  const limit = ${LIMIT};
  globalThis.__tiloLargestBuffer = 0;
  const requestDevice = GPUAdapter.prototype.requestDevice;
  GPUAdapter.prototype.requestDevice = function (descriptor = {}) {
    const requiredLimits = { ...(descriptor.requiredLimits ?? {}) };
    for (const key of ['maxStorageBufferBindingSize', 'maxBufferSize']) {
      requiredLimits[key] = Math.min(requiredLimits[key] ?? limit, limit);
    }
    return requestDevice.call(this, { ...descriptor, requiredLimits });
  };
  const createBuffer = GPUDevice.prototype.createBuffer;
  GPUDevice.prototype.createBuffer = function (descriptor) {
    globalThis.__tiloLargestBuffer = Math.max(globalThis.__tiloLargestBuffer, Number(descriptor.size));
    return createBuffer.call(this, descriptor);
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
  if (msg.method === 'Runtime.exceptionThrown') log.push('EXC ' + (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text).slice(0, 400));
  if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') log.push('ERROR ' + msg.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 400));
});
await send('Runtime.enable', {}, sessionId);
await send('Page.enable', {}, sessionId);
await send('Page.addScriptToEvaluateOnNewDocument', {source: clamp}, sessionId);
const [width, height] = (process.env.VIEWPORT ?? '1728x1117').split('x').map(Number);
await send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: Number(process.env.DPR ?? 2), mobile: false}, sessionId);
await send('Page.navigate', {url: `${base}/`}, sessionId);

const status = () => evaluate("(document.documentElement.dataset.phase ?? '') + ' · frames ' + (document.documentElement.dataset.frames ?? 0) + ' · ' + (document.getElementById('status')?.textContent ?? '')");
const frames = text => Number(/frames (\d+)/.exec(text)?.[1] ?? 0);
const failed = text => /^failed|Failed/.test(text);
let text = '';
for (let i = 0; i < 90; i++) {
  await sleep(1000);
  text = await status();
  if (failed(text)) break;
  if (frames(text) > 0 && await evaluate('Boolean(globalThis.tiloMount?.navigation)')) break;
}
if (!failed(text)) {
  const apps = await evaluate(`fetch('apps.json').then(r => r.json()).then(j => j.applications.map(a => a.id))`);
  for (let i = 0; i < windows && !failed(text); i++) {
    const app = apps[i % apps.length];
    await evaluate(`(() => { const n = globalThis.tiloMount.navigation; return n.dispatch({kind: 'launch', generation: n.getSnapshot().generation, app: ${JSON.stringify(app)}}); })()`)
      .catch(error => log.push(`launch ${app}: ${error.message}`));
    await sleep(400);
    text = await status();
  }
  // Let deferred tabs build and the renderer's buffers settle.
  await sleep(8000);
  text = await status();
}
const settled = frames(text);
const entries = await evaluate('globalThis.tiloMount?.navigation?.getSnapshot().entries?.length ?? 0').catch(() => 0);
await sleep(2000);
text = await status();
const largest = await evaluate('globalThis.__tiloLargestBuffer');
const shots = process.env.SHOTS_DIR;
if (shots) {
  const shot = await send('Page.captureScreenshot', {format: 'png'}, sessionId);
  if (shot.result?.data) (await import('node:fs')).writeFileSync(`${shots}/device-limits.png`, Buffer.from(shot.result.data, 'base64'));
}
console.log(JSON.stringify({windows, entries, status: text, largestBuffer: largest, limit: LIMIT, log: log.slice(0, 12)}));
await send('Target.closeTarget', {targetId});
ws.close();
const problems = [];
if (!/^presented/.test(text)) problems.push(`desktop is not presenting: ${text}`);
if (entries < windows) problems.push(`only ${entries} of ${windows} windows opened`);
if (settled === 0) problems.push('no frame was presented');
if (largest > LIMIT) problems.push(`created a ${largest}-byte buffer past the ${LIMIT}-byte limit`);
if (log.some(l => l.startsWith('EXC') || /panic/i.test(l))) problems.push('the page threw or panicked');
for (const p of problems) console.error(`FAIL ${p}`);
process.exit(problems.length ? 1 : 0);
