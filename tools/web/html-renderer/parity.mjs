// Every app of the desktop bundle shown as HTML against the same app on the
// WebGPU canvas: a pixel diff of the first settled frame, where the semantic
// elements live, the fields the page shows as its own inputs, and the
// latency of typing into the app's first text field and of wheel steps over
// its middle, against the plain-DOM page in the same run. Both renders get
// the light theme as the person's setting, so an app that shows which theme
// is in use shows the same one.
//
// usage: node tools/web/html-renderer/parity.mjs BUNDLE_DIR OUT_DIR [--apps id,id]
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';
import {decode, diff} from './png.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
mkdirSync(out, {recursive: true});
const only = process.argv.includes('--apps') ? process.argv[process.argv.indexOf('--apps') + 1].split(',') : null;
const apps = JSON.parse(readFileSync(join(bundle, 'apps.json'), 'utf8')).applications
  .map(app => app.id).filter(id => !only || only.includes(id));
const gpuArgs = process.platform === 'linux' ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader'] : [];
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];
const [width, height] = [1280, 800];

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};

// In the page: a mark at the first animation frame after each change, as
// measure.mjs takes it.
const INSTRUMENT = `(() => {
  const m = window.renderMarks ??= {presented: []};
  m.inputs = []; m.painted = [];
  const changed = () => requestAnimationFrame(() => m.painted.push(performance.now()));
  if (m.presented) { const push = m.presented.push.bind(m.presented); m.presented.push = (...v) => { changed(); return push(...v); }; }
  for (const type of ['keydown', 'wheel']) addEventListener(type, e => m.inputs.push([type, e.timeStamp]), {capture: true, passive: true});
  document.addEventListener('scroll', changed, true);
  document.addEventListener('input', changed, true);
  return true;
})()`;
const LATENCIES = `(() => {
  const m = window.renderMarks, out = {keydown: [], wheel: []};
  for (const [type, at] of m.inputs) { const painted = m.painted.find(t => t >= at); if (painted !== undefined) out[type].push(painted - at); }
  m.inputs = []; m.painted = [];
  return JSON.stringify(out);
})()`;
// The first text field on screen: its centre, its role, and whether the page
// shows it as its own input.
const FIELD = `(() => {
  const walk = n => { if (!n) return null;
    const s = n.semantics;
    if (s && ['TextInput', 'MultilineTextInput'].includes(s.role) && !s.state?.disabled && n.rect && n.rect[2] > 20 && n.rect[3] > 8
      && n.rect[0] >= 0 && n.rect[1] >= 0 && n.rect[0] + n.rect[2] <= innerWidth && n.rect[1] + n.rect[3] <= innerHeight)
      return {x: n.rect[0] + Math.min(n.rect[2] / 2, 60), y: n.rect[1] + n.rect[3] / 2, role: s.role, autofill: s.state?.autofill ?? null};
    for (const c of n.children ?? []) { const r = walk(c); if (r) return r; } return null; };
  return JSON.stringify(walk(window.mount.state()?.semantics?.root));
})()`;
const PAGE = `JSON.stringify({
  elements: document.querySelectorAll('.tilo-dom *').length,
  texts: document.querySelectorAll('.tilo-dom span').length,
  hosted: document.querySelectorAll('.tilo-dom [data-tilo-semantic]').length,
  mirrored: document.querySelectorAll('[data-tilo-semantic]').length - document.querySelectorAll('.tilo-dom [data-tilo-semantic]').length,
  paintedTextsExposed: [...document.querySelectorAll('.tilo-dom span:not([data-tilo-semantic])')].filter(s => !s.closest('[aria-hidden="true"]')).length,
  fields: [...document.querySelectorAll('input[data-tilo-field]')].map(i => ({type: i.type, autocomplete: i.getAttribute('autocomplete')})),
  placeholders: document.querySelectorAll('.tilo-dom div[style*="repeating-linear-gradient"]').length,
  frameMs: window.__tiloDom?.stats.lastMs ?? null,
})`;

async function settle(tab) {
  await tab.waitFor('!!window.renderMarks', {timeout: 60000});
  await tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
  let last = -1;
  for (let i = 0; i < 40; i++) {
    await sleep(250);
    const n = await tab.evaluate('window.renderMarks.presented.length');
    if (n === last) break;
    last = n;
  }
}

async function open(server, page) {
  const browser = await launch({args: [`--window-size=${width},${height}`, ...gpuArgs, ...sandbox]});
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 1, mobile: false});
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: 'light'}]});
  await tab.navigate(`${server.url}/${page}`);
  return {browser, tab};
}

// Typing into the first field and wheel steps over the middle: each input's
// latency to the first painted change after it.
async function interact(tab, field) {
  await tab.evaluate(INSTRUMENT);
  const result = {};
  if (field) {
    for (const type of ['mousePressed', 'mouseReleased']) await tab.send('Input.dispatchMouseEvent', {type, x: field.x, y: field.y, button: 'left', clickCount: 1});
    await sleep(400);
    await tab.evaluate(LATENCIES);
    for (const ch of 'Parity typing') {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
      await sleep(60);
    }
    await sleep(300);
    const typed = JSON.parse(await tab.evaluate(LATENCIES)).keydown;
    result.typing = {n: typed.length, p50: pct(typed, 0.5), p90: pct(typed, 0.9)};
    result.focus = await tab.evaluate('document.activeElement?.localName ?? null');
  }
  await tab.evaluate(LATENCIES);
  for (let i = 0; i < 12; i++) {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: width / 2, y: height / 2, deltaX: 0, deltaY: i < 6 ? 120 : -120});
    await sleep(60);
  }
  await sleep(300);
  const wheel = JSON.parse(await tab.evaluate(LATENCIES)).wheel;
  result.wheel = {n: wheel.length, p50: pct(wheel, 0.5), p90: pct(wheel, 0.9)};
  return result;
}

const server = await serve(bundle);
const results = [];
try {
  // The plain-DOM page: what typing and scrolling should feel like.
  {
    const {browser, tab} = await open(server, 'baseline.html');
    try {
      await sleep(1000);
      const baseline = {app: 'baseline'};
      baseline.interaction = await interact(tab, {x: 600, y: 120});
      console.log(JSON.stringify(baseline));
      results.push(baseline);
    } finally { await browser.close().catch(() => {}); }
  }
  for (const app of apps) {
    const result = {app};
    for (const render of ['dom', 'gpu']) {
      const {browser, tab} = await open(server, `app.html?app=${app}&render=${render}&theme=light`);
      try {
        await settle(tab);
        writeFileSync(join(out, `${app}-${render}.png`), await tab.screenshot());
        if (render === 'dom') {
          result.page = JSON.parse(await tab.evaluate(PAGE));
          result.field = JSON.parse(await tab.evaluate(FIELD));
          result.interaction = await interact(tab, result.field);
          result.pageAfter = JSON.parse(await tab.evaluate(PAGE));
          writeFileSync(join(out, `${app}-dom-after.png`), await tab.screenshot());
        }
        const errors = tab.log.filter(line => /^(EXC|ERROR)/.test(line)).slice(0, 6);
        if (errors.length) result[`${render}Errors`] = errors;
      } catch (error) {
        result[`${render}Error`] = String(error?.stack ?? error).slice(0, 2000);
      } finally { await browser.close().catch(() => {}); }
    }
    try {
      const compared = diff(decode(readFileSync(join(out, `${app}-dom.png`))), decode(readFileSync(join(out, `${app}-gpu.png`))));
      if (compared.image) writeFileSync(join(out, `${app}-diff.png`), compared.image);
      result.parity = {differingShare: compared.differingShare, meanDifference: compared.meanDifference, error: compared.error};
    } catch (error) { result.parity = {error: String(error)}; }
    console.log(JSON.stringify(result));
    results.push(result);
    writeFileSync(join(out, 'parity.json'), JSON.stringify(results, null, 1));
  }
} finally {
  await server.close();
}
