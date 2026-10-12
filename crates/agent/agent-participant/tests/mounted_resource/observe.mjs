/** Main-thread evidence for a mounted journey: every page stall, the long
 * animation frames that caused it, and when each mount first presented.
 * With TILO_MOUNTED_TRACE set, also a Chromium performance trace (renderer,
 * V8 sampling, GPU) of the whole journey. */
import {createWriteStream, writeFileSync} from 'node:fs';
import {join} from 'node:path';

const PAGE = `(() => {
  const send = row => { try { globalThis.__tiloObserve?.(JSON.stringify({url: location.pathname, ...row})); } catch {} };
  const epoch = t => Math.round(performance.timeOrigin + t);
  let last = performance.now();
  setInterval(() => {
    const now = performance.now();
    if (now - last > 400) send({kind: 'stall', at: epoch(last), ms: Math.round(now - last)});
    last = now;
  }, 100);
  try {
    new PerformanceObserver(list => {
      for (const frame of list.getEntries()) {
        if (frame.duration < 400) continue;
        send({kind: 'frame', at: epoch(frame.startTime), ms: Math.round(frame.duration),
          blockingMs: Math.round(frame.blockingDuration ?? 0),
          renderMs: Math.round(frame.renderStart ? frame.startTime + frame.duration - frame.renderStart : 0),
          styleLayoutMs: Math.round(frame.styleAndLayoutStart ? frame.startTime + frame.duration - frame.styleAndLayoutStart : 0),
          scripts: (frame.scripts ?? []).map(script => ({invoker: script.invoker, type: script.invokerType,
            source: script.sourceURL?.split('/').pop(), fn: script.sourceFunctionName,
            ms: Math.round(script.duration), layoutMs: Math.round(script.forcedStyleAndLayoutDuration ?? 0)}))});
      }
    }).observe({type: 'long-animation-frame', buffered: true});
  } catch {}
  const seen = new WeakSet();
  new MutationObserver(() => {
    for (const host of document.querySelectorAll('.tilo-host[data-phase="presented"]')) {
      if (seen.has(host)) continue;
      seen.add(host);
      send({kind: 'presented', at: epoch(performance.now())});
    }
  }).observe(document, {subtree: true, attributes: true, attributeFilter: ['data-phase']});
})();`;

const CATEGORIES = ['devtools.timeline', 'disabled-by-default-devtools.timeline',
  'disabled-by-default-devtools.timeline.frame', 'v8', 'v8.wasm', 'disabled-by-default-v8.compile',
  'disabled-by-default-v8.cpu_profiler', 'blink', 'blink.user_timing', 'gpu', 'gpu.dawn', 'toplevel', 'loading'];

export async function observe(browser, tab, evidence) {
  const rows = [];
  const stop = browser.on(message => {
    if (message.sessionId === tab.sessionId && message.method === 'Runtime.bindingCalled'
      && message.params.name === '__tiloObserve') {
      const row = JSON.parse(message.params.payload);
      rows.push(row);
      if (row.kind === 'presented' || (row.kind === 'stall' && row.ms >= 1_000)) {
        process.stderr.write(`page ${row.kind}${row.ms ? ` ${row.ms} ms` : ''} on ${row.url} at ${row.at}\n`);
      }
    }
  });
  await tab.send('Runtime.addBinding', {name: '__tiloObserve'});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: PAGE});
  const tracing = Boolean(process.env.TILO_MOUNTED_TRACE);
  if (tracing) {
    const started = await browser.send('Tracing.start', {transferMode: 'ReturnAsStream',
      streamFormat: 'json', streamCompression: 'gzip',
      traceConfig: {recordMode: 'recordAsMuchAsPossible', includedCategories: CATEGORIES}});
    if (started.error) throw Error('tracing unavailable: ' + JSON.stringify(started.error));
  }
  const marks = [];
  return {
    mark(name) { marks.push({name, at: Date.now()}); },
    /** The longest time the page could not run a timer, and where. */
    longestStall() {
      return rows.filter(row => row.kind === 'stall')
        .reduce((longest, row) => row.ms > longest.ms ? row : longest, {ms: 0, url: 'no page'});
    },
    /** Mounts whose first frame the page reported presenting. */
    presentedMounts() { return rows.filter(row => row.kind === 'presented').length; },
    async finish() {
      stop();
      writeFileSync(join(evidence, 'main-thread.json'), JSON.stringify({marks, rows}, null, 2) + '\n');
      if (!tracing) return;
      const complete = new Promise(resolve => {
        const off = browser.on(message => {
          if (message.method === 'Tracing.tracingComplete') { off(); resolve(message.params); }
        });
      });
      await browser.send('Tracing.end');
      const {stream} = await complete;
      const out = createWriteStream(join(evidence, 'trace.json.gz'));
      for (;;) {
        const {result} = await browser.send('IO.read', {handle: stream, size: 4 << 20});
        out.write(Buffer.from(result.data, result.base64Encoded ? 'base64' : 'utf8'));
        if (result.eof) break;
      }
      await browser.send('IO.close', {handle: stream});
      await new Promise(done => out.end(done));
    },
  };
}
