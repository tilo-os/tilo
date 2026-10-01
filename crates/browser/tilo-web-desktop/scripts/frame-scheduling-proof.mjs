// Live regression proof against a built desktop bundle. Uses a fresh Chromium
// context and the same real launch-to-present path as open-times.mjs.
// CDP_PORT=9333 node frame-scheduling-proof.mjs http://127.0.0.1:8765/
const base = process.argv[2];
if (!base) throw Error('A built desktop bundle URL is required');
const pageURL = new URL('index.html', base.endsWith('/') ? base : `${base}/`).href;
const version = await (await fetch(`http://127.0.0.1:${process.env.CDP_PORT ?? 9333}/json/version`)).json();
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise(resolve => ws.addEventListener('open', resolve, {once: true}));
let serial = 0;
const pending = new Map();
ws.addEventListener('message', ({data}) => {
  const message = JSON.parse(data), resolve = pending.get(message.id);
  if (resolve) { pending.delete(message.id); resolve(message); }
});
const send = (method, params = {}, sessionId) => new Promise(resolve => {
  const id = ++serial; pending.set(id, resolve);
  ws.send(JSON.stringify({id, method, params, sessionId}));
});
const {result: {browserContextId}} = await send('Target.createBrowserContext', {disposeOnDetach: true});
try {
  const {result: {targetId}} = await send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
  const evaluate = async expression => {
    const reply = await send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}, sessionId);
    if (reply.error || reply.result?.exceptionDetails) throw Error(JSON.stringify(reply));
    return reply.result.result.value;
  };
  await send('Page.enable', {}, sessionId);
  await send('Page.navigate', {url: pageURL}, sessionId);
  for (let i = 0; i < 600 && !await evaluate('Boolean(globalThis.tiloMount?.navigation)'); i++) await new Promise(resolve => setTimeout(resolve, 100));
  if (!await evaluate('Boolean(globalThis.tiloMount?.navigation)')) throw Error('Desktop did not present');
  await new Promise(resolve => setTimeout(resolve, 1000));
  const result = await evaluate(`(async () => {
    const runtime = await import('./pkg/tilo_browser_runtime.js');
    const mount = tiloMount, host = mount.canvas.parentElement;
    async function launch(app) {
      const snapshot = mount.navigation.getSnapshot(), frames = Number(host.dataset.frames);
      let observer;
      const presented = new Promise(resolve => {
        observer = new MutationObserver(() => {
          if (host.dataset.phase === 'presented' && Number(host.dataset.frames) > frames) { observer.disconnect(); resolve(); }
        });
        observer.observe(host, {attributes: true, attributeFilter: ['data-frames']});
      });
      try {
        await mount.navigation.dispatch({kind: 'launch', generation: snapshot.generation, app});
        await Promise.race([presented, new Promise((_, reject) => setTimeout(() => reject(Error('No presented launch frame')), 10000))]);
        const after = mount.navigation.getSnapshot();
        if (after.entries.length !== snapshot.entries.length + 1 || !after.active || after.active.tab === snapshot.active?.tab) throw Error('Launch did not open a new active window');
      } finally { observer.disconnect(); }
    }
    runtime.configure_frame_metrics(true);
    runtime.take_frame_metrics();
    await launch('so.tilo.calculator');
    const enabled = JSON.parse(runtime.take_frame_metrics());
    runtime.configure_frame_metrics(false);
    await launch('so.tilo.calculator');
    const disabled = JSON.parse(runtime.take_frame_metrics());
    return {enabled, disabled};
  })()`);
  const errors = [];
  if (!result.enabled.some(mount => mount.samples.some(sample => sample.stage === 'draw_total'))) errors.push('No measured production redraw');
  for (const mount of result.enabled) {
    let layouts = 0;
    for (const sample of mount.samples) {
      if (sample.stage === 'wake_relayout' || /poll_frame:(rebuilt|measured|applied)/.test(sample.stage)) layouts++;
      if (sample.stage === 'draw_total') {
        if (layouts > 1) errors.push(`Redraw ran ${layouts} layout passes`);
        layouts = 0;
      }
    }
  }
  if (result.disabled.some(mount => mount.samples.length || mount.layoutPasses.length)) errors.push('Disabled profiling still records frame work');
  console.log(JSON.stringify({errors, ...result}));
  if (errors.length) process.exitCode = 1;
} finally {
  await send('Target.disposeBrowserContext', {browserContextId});
  ws.close();
}
