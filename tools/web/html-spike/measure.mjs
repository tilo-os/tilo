// Measures the Notes app shown as HTML against the same app on the WebGPU
// canvas and a plain-DOM page: transfer size, time to first frame, typing and
// scrolling latency, memory, and screenshots at phone and desktop widths.
//
// usage: node tools/web/html-spike/measure.mjs BUNDLE_DIR OUT_DIR [--rounds N]
import {mkdirSync, readFileSync, readdirSync, writeFileSync, existsSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {brotliCompressSync, gzipSync, constants as z} from 'node:zlib';
import {launch, serve, sleep} from '../chrome.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
const rounds = Number(process.argv[process.argv.indexOf('--rounds') + 1]) || 1;
mkdirSync(out, {recursive: true});
const gpuArgs = process.platform === 'linux'
  ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']
  : [];
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};

// Every process below this one: the browser, its renderers and GPU process.
function descendants() {
  const children = new Map();
  for (const pid of readdirSync('/proc').filter(n => /^\d+$/.test(n))) {
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, 'utf8');
      const ppid = Number(stat.slice(stat.lastIndexOf(')') + 2).split(' ')[1]);
      if (!children.has(ppid)) children.set(ppid, []);
      children.get(ppid).push(Number(pid));
    } catch {}
  }
  const all = [];
  const walk = pid => { for (const child of children.get(pid) ?? []) { all.push(child); walk(child); } };
  walk(process.pid);
  return all;
}

function memory() {
  const byType = {};
  for (const pid of descendants()) {
    try {
      const cmd = readFileSync(`/proc/${pid}/cmdline`, 'utf8');
      if (!/chrome/.test(cmd)) continue;
      const type = cmd.match(/--type=([a-z-]+)/)?.[1] ?? 'browser';
      const rollup = readFileSync(`/proc/${pid}/smaps_rollup`, 'utf8');
      const pss = Number(rollup.match(/^Pss:\s+(\d+)/m)?.[1] ?? 0);
      byType[type] = (byType[type] ?? 0) + pss;
    } catch {}
  }
  const total = Object.values(byType).reduce((a, b) => a + b, 0);
  return {totalMB: Math.round(total / 1024), byTypeMB: Object.fromEntries(Object.entries(byType).map(([k, v]) => [k, Math.round(v / 1024)]))};
}

// What the page transferred, by kind, raw and compressed as a deployment serves it.
function transfer(names) {
  const sizes = {wasm: [0, 0, 0], js: [0, 0, 0], fonts: [0, 0, 0], other: [0, 0, 0]};
  for (const name of new Set(names)) {
    const path = join(bundle, new URL(name).pathname);
    if (!existsSync(path)) continue;
    const bytes = readFileSync(path);
    const kind = /\.wasm$/.test(path) ? 'wasm' : /\.m?js$/.test(path) ? 'js' : /\.(ttf|otf|woff2?)$/.test(path) ? 'fonts' : 'other';
    sizes[kind][0] += bytes.length;
    sizes[kind][1] += gzipSync(bytes, {level: 9}).length;
    sizes[kind][2] += brotliCompressSync(bytes, {params: {[z.BROTLI_PARAM_QUALITY]: 11, [z.BROTLI_PARAM_SIZE_HINT]: bytes.length}}).length;
  }
  return Object.fromEntries(Object.entries(sizes).map(([k, [raw, gz, br]]) => [k, {raw, gzip: gz, brotli: br}]));
}

// In the page: a mark at the first animation frame after each change, which
// is when the browser paints it.
const INSTRUMENT = `(() => {
  const m = window.spikeMarks;
  m.inputs = []; m.painted = [];
  const changed = () => requestAnimationFrame(() => { const t = performance.now(); m.painted.push(t); });
  const push = m.presented.push.bind(m.presented);
  m.presented.push = (...v) => { changed(); return push(...v); };
  for (const type of ['keydown', 'wheel']) addEventListener(type, e => m.inputs.push([type, e.timeStamp]), {capture: true, passive: true});
  if (location.pathname.endsWith('baseline.html')) {
    document.addEventListener('input', changed, true);
    document.addEventListener('scroll', changed, true);
  }
  return true;
})()`;

const MARKDOWN = Array.from({length: 60}, (_, i) => i % 10 === 0
  ? `## Section ${i / 10 + 1}`
  : `${i % 3 === 0 ? '- ' : ''}Paragraph ${i}: the quick brown fox jumps over the lazy dog while notes keep every thought close at hand, and the page keeps scrolling.`).join('\n\n');
const SEED = `(async () => {
  const actions = window.mount.actions();
  const importing = actions.find(a => /import/i.test(a.address.action) && /markdown/i.test(a.address.action));
  if (!importing) return 'no import action: ' + actions.map(a => a.address.action).join(',');
  const answer = await window.mount.invokeAction(importing.address.resource, importing.address.action,
    {title: 'Field notes', markdown: ${JSON.stringify(JSON.stringify(MARKDOWN))}.length ? JSON.parse(${JSON.stringify(JSON.stringify(MARKDOWN))}) : '', images: [], note: null});
  return JSON.stringify(answer);
})()`;
// The centre of the first semantic node whose label or value contains `text`.
const FIND = text => `(() => {
  const root = window.mount.state()?.semantics?.root;
  const walk = n => { if (!n) return null;
    const s = n.semantics; const words = [s?.label, s?.value, s?.name].filter(Boolean).join(' ');
    if (words.includes(${JSON.stringify(text)}) && n.rect) return [n.rect[0] + Math.min(n.rect[2] / 2, 80), n.rect[1] + n.rect[3] / 2];
    for (const c of n.children ?? []) { const r = walk(c); if (r) return r; } return null; };
  return walk(root);
})()`;

// Each input's latency: from its event to the first painted change after it.
const LATENCIES = `(() => {
  const m = window.spikeMarks, out = {keydown: [], wheel: []};
  for (const [type, at] of m.inputs) {
    const painted = m.painted.find(t => t >= at);
    if (painted !== undefined) out[type].push(painted - at);
  }
  m.inputs = []; m.painted = [];
  return out;
})()`;

async function run({page, render, width, height, dark = false, shot, interact = true}) {
  const browser = await launch({args: [`--window-size=${width},${height}`, ...gpuArgs, ...sandbox]});
  const server = await serve(bundle);
  const result = {page, render, width, height, dark};
  try {
    const tab = await browser.tab();
    await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: width < 500 ? 3 : 1, mobile: width < 500});
    if (width < 500) await tab.send('Emulation.setTouchEmulationEnabled', {enabled: true, maxTouchPoints: 5});
    await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: dark ? 'dark' : 'light'}]});
    await tab.send('Performance.enable', {});
    const url = `${server.url}/${page}${render ? `?render=${render}&theme=${dark ? 'dark' : 'light'}` : ''}`;
    await tab.navigate(url);
    await tab.waitFor('!!window.spikeMarks', {timeout: 60000});
    if (page === 'notes.html') {
      const first = await tab.waitFor('window.spikeMarks.presented[0]', {timeout: 90000});
      result.firstFrameMs = Math.round(first);
      // Let startup settle: fonts, the notebook's first sync.
      let last = -1;
      for (let i = 0; i < 40; i++) {
        await sleep(250);
        const n = await tab.evaluate('window.spikeMarks.presented.length');
        if (n === last) break;
        last = n;
      }
      // Live data: a long note imported through the app's own action, then
      // opened from the sidebar with a click, as a person would.
      result.seed = await tab.evaluate(SEED).catch(error => String(error));
      await sleep(1500);
      const item = await tab.evaluate(FIND('Field notes'));
      result.seedItem = item;
      if (item) {
        for (const type of ['mousePressed', 'mouseReleased']) {
          await tab.send('Input.dispatchMouseEvent', {type, x: item[0], y: item[1], button: 'left', clickCount: 1});
        }
        await sleep(1500);
      }
      result.dom = await tab.evaluate('JSON.stringify(window.spikeMarks.dom?.meta ?? null)').then(JSON.parse);
      result.domElements = await tab.evaluate('document.querySelectorAll(".tilo-dom *").length');
      result.domTexts = await tab.evaluate('document.querySelectorAll(".tilo-dom span").length');
    } else {
      result.firstFrameMs = Math.round(await tab.evaluate('performance.getEntriesByType("paint").find(e=>e.name==="first-contentful-paint")?.startTime ?? 0'));
    }
    if (shot) writeFileSync(join(out, shot), await tab.screenshot());
    const names = await tab.evaluate('JSON.stringify([location.href, ...performance.getEntriesByType("resource").map(e => e.name)])').then(JSON.parse);
    result.transfer = transfer(names);
    if (interact) {
      await tab.evaluate(INSTRUMENT);
      // Put the caret at the end of the document's text: a click into the
      // editor's body, as a person would.
      const target = page === 'baseline.html'
        ? {x: width < 500 ? width / 2 : 600, y: 120}
        : {x: width < 500 ? width / 2 : Math.round(width * 0.6), y: Math.round(height * 0.35)};
      for (const type of ['mousePressed', 'mouseReleased']) {
        await tab.send('Input.dispatchMouseEvent', {type, x: target.x, y: target.y, button: 'left', clickCount: 1});
      }
      await sleep(400);
      if (page === 'baseline.html') await tab.evaluate('(()=>{const s=getSelection();s.selectAllChildren(document.querySelector("#doc p"));s.collapseToEnd();return true})()');
      await tab.evaluate(LATENCIES);
      const text = 'Typing into the page measures each frame. ';
      for (let r = 0; r < rounds; r++) {
        for (const ch of text) {
          await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
          await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
          await sleep(60);
        }
      }
      await sleep(300);
      const typed = await tab.evaluate(LATENCIES);
      result.typing = {n: typed.keydown.length, p50: pct(typed.keydown, 0.5), p90: pct(typed.keydown, 0.9), max: pct(typed.keydown, 1)};
      if (shot) writeFileSync(join(out, shot.replace('.png', '-typed.png')), await tab.screenshot());
      for (let i = 0; i < 30 * rounds; i++) {
        await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: target.x, y: target.y, deltaX: 0, deltaY: i % 20 < 10 ? 120 : -120});
        await sleep(60);
      }
      await sleep(300);
      const scrolled = await tab.evaluate(LATENCIES);
      result.scrolling = {n: scrolled.wheel.length, p50: pct(scrolled.wheel, 0.5), p90: pct(scrolled.wheel, 0.9), max: pct(scrolled.wheel, 1)};
      if (page === 'notes.html') result.domAfter = await tab.evaluate('JSON.stringify(window.spikeMarks.dom ?? null)').then(JSON.parse);
    }
    const metrics = Object.fromEntries((await tab.send('Performance.getMetrics', {})).result.metrics.map(m => [m.name, m.value]));
    result.jsHeapMB = Math.round(metrics.JSHeapUsedSize / 1048576);
    result.nodes = metrics.Nodes;
    result.memory = memory();
    result.log = tab.log.slice(0, 20);
  } catch (error) {
    result.error = String(error?.stack ?? error);
  } finally {
    await server.close();
    await browser.close().catch(() => {});
  }
  console.log(JSON.stringify(result));
  return result;
}

const results = [];
// The server compresses each file on its first request; a throwaway load
// keeps that out of the first measured run.
await run({page: 'notes.html', render: 'dom', width: 1280, height: 800, interact: false});
results.push(await run({page: 'notes.html', render: 'dom', width: 1280, height: 800, shot: 'notes-dom-1280.png'}));
results.push(await run({page: 'notes.html', render: 'gpu', width: 1280, height: 800, shot: 'notes-gpu-1280.png'}));
results.push(await run({page: 'baseline.html', width: 1280, height: 800, shot: 'baseline-1280.png'}));
results.push(await run({page: 'notes.html', render: 'dom', width: 390, height: 844, shot: 'notes-dom-390.png'}));
results.push(await run({page: 'notes.html', render: 'gpu', width: 390, height: 844, shot: 'notes-gpu-390.png', interact: false}));
results.push(await run({page: 'notes.html', render: 'dom', width: 1280, height: 800, dark: true, shot: 'notes-dom-1280-dark.png', interact: false}));
results.push(await run({page: 'notes.html', render: 'dom', width: 390, height: 844, dark: true, shot: 'notes-dom-390-dark.png', interact: false}));
writeFileSync(join(out, 'results.json'), JSON.stringify(results, null, 1));
