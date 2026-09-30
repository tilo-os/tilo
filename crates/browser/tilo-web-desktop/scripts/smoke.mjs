// Mount each application of a built desktop bundle in headless Chromium on
// WebGPU and report its status line, console errors and exceptions; writes a
// screenshot per app to SHOTS_DIR (default: the current directory).
//
// usage: node smoke.mjs BASE_URL APP_ID... [desktop]
//   BASE_URL  where build.sh's output is served (any static server)
//   Chromium  started with --headless=new --enable-unsafe-webgpu
//             --remote-debugging-port=${CDP_PORT:-9333}
// Exits non-zero when any app panics, throws or never presents a frame.
const [base, ...apps] = process.argv.slice(2);
const port = process.env.CDP_PORT ?? '9333';
const shots = process.env.SHOTS_DIR ?? '.';
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

const results = [];
for (const app of apps) {
  const {result: {targetId}} = await send('Target.createTarget', {url: 'about:blank'});
  const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
  const log = [];
  const onMsg = msg => {
    if (msg.sessionId !== sessionId) return;
    if (msg.method === 'Runtime.exceptionThrown') log.push('EXC ' + (msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text).slice(0, 400));
    if (msg.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(msg.params.type)) log.push(msg.params.type.toUpperCase() + ' ' + msg.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 400));
  };
  listeners.push(onMsg);
  await send('Runtime.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  const url = app === 'desktop' ? `${base}/index.html` : `${base}/index.html?app=${encodeURIComponent(app)}`;
  await send('Page.navigate', {url}, sessionId);
  let status = '';
  for (let i = 0; i < 90; i++) {
    await sleep(1000);
    const r = await send('Runtime.evaluate', {expression: "document.getElementById('status')?.textContent ?? ''", returnByValue: true}, sessionId);
    status = r.result?.result?.value ?? '';
    if (/frames [1-9]/.test(status) && i > 5) break;
    if (/Failed|error/i.test(status) || log.some(l => l.startsWith('EXC'))) break;
  }
  // Let a few more frames run so construction-time work has happened.
  await sleep(3000);
  const r = await send('Runtime.evaluate', {expression: "document.getElementById('status')?.textContent ?? ''", returnByValue: true}, sessionId);
  status = r.result?.result?.value ?? status;
  const shot = await send('Page.captureScreenshot', {format: 'png'}, sessionId);
  if (shot.result?.data) (await import('node:fs')).writeFileSync(`${shots}/${app}.png`, Buffer.from(shot.result.data, 'base64'));
  results.push({app, status, log: log.slice(0, 8)});
  console.log(JSON.stringify({app, status, log: log.slice(0, 8)}));
  listeners.splice(listeners.indexOf(onMsg), 1);
  await send('Target.closeTarget', {targetId});
}
ws.close();
const failed = results.filter(r => !/^presented/.test(r.status) || r.log.some(l => l.startsWith('EXC') || /panic/i.test(l)));
for (const r of failed) console.error(`FAIL ${r.app}: ${r.status}`);
process.exit(failed.length ? 1 : 0);
