// Drag a Notes block by its six-dot handle on the browser desktop, as a mouse
// does: press on the handle, many small moves, release. The ghost follows the
// pointer on every step, the drop indicator only moves forward through the
// blocks (no oscillation), and the release moves the block there. Reports the
// ghost and indicator position per step and the frames presented per move.
//
// usage: node block-drag.mjs BUNDLE_DIR [--webkit]
//   BUNDLE_DIR  build.sh --bundle desktop output
//   --webkit    Playwright's WebKit instead of headless Chrome; resolves
//               playwright-core from $PLAYWRIGHT_CORE, else from this tree
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {inflateSync} from 'node:zlib';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const args = process.argv.slice(2);
const bundle = args.find(arg => !arg.startsWith('--'));
if (!bundle) throw Error('usage: node block-drag.mjs BUNDLE_DIR [--webkit]');
const webkit = args.includes('--webkit');
const server = await serve(resolve(bundle));
const VIEWPORT = {width: 1280, height: 800};

async function chrome() {
  const browser = await launch();
  const tab = await browser.tab();
  await tab.send('Emulation.setDeviceMetricsOverride', {...VIEWPORT, deviceScaleFactor: 1, mobile: false});
  await tab.navigate(`${server.url}/index.html`);
  let buttons = 0;
  const mouse = (type, x, y) => tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', buttons, clickCount: 1});
  return {
    evaluate: tab.evaluate,
    errors: () => tab.log.filter(line => line.startsWith('EXC')),
    move: (x, y) => mouse('mouseMoved', x, y),
    down: (x, y) => { buttons = 1; return mouse('mousePressed', x, y); },
    up: (x, y) => { buttons = 0; return mouse('mouseReleased', x, y); },
    type: async text => { for (const ch of text) { await tab.send('Input.insertText', {text: ch}); await sleep(15); } },
    enter: async () => {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r'});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13});
    },
    shot: () => tab.screenshot(),
    close: () => browser.close(),
  };
}
async function playwrightWebkit() {
  const core = process.env.PLAYWRIGHT_CORE ?? 'playwright-core';
  const {webkit: engine} = await import(core.startsWith('/') ? pathToFileURL(resolve(core, 'index.mjs')).href : core);
  // A persistent (throwaway) profile: WebKit refuses the runtime's storage
  // in an ephemeral one.
  const profile = mkdtempSync(join(tmpdir(), 'tilo-webkit-'));
  const context = await engine.launchPersistentContext(profile, {viewport: VIEWPORT});
  const page = context.pages()[0] ?? await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(`EXC ${error.message}`));
  await page.goto(`${server.url}/index.html`);
  return {
    evaluate: expression => page.evaluate(expression),
    errors: () => errors,
    move: (x, y) => page.mouse.move(x, y),
    down: async (x, y) => { await page.mouse.move(x, y); await page.mouse.down(); },
    up: async (x, y) => { await page.mouse.move(x, y); await page.mouse.up(); },
    type: text => page.keyboard.type(text, {delay: 15}),
    enter: () => page.keyboard.press('Enter'),
    shot: () => page.screenshot(),
    close: async () => { await context.close(); rmSync(profile, {recursive: true, force: true}); },
  };
}

// 8-bit RGB(A), non-interlaced PNG, as both engines capture: {width, height, at(x, y) -> [r, g, b]}.
function decodePng(png) {
  let offset = 8, width, height, channels;
  const idat = [];
  while (offset < png.length) {
    const length = png.readUInt32BE(offset), type = png.toString('ascii', offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0); height = data.readUInt32BE(4);
      assert.equal(data[8], 8, 'an 8-bit capture');
      channels = {2: 3, 6: 4}[data[9]];
      assert.ok(channels && data[12] === 0, `an RGB(A) non-interlaced capture: ${data[9]}/${data[12]}`);
    }
    if (type === 'IDAT') idat.push(data);
    offset += 12 + length;
  }
  const raw = inflateSync(Buffer.concat(idat)), stride = width * channels, pixels = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)], line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let i = 0; i < stride; i++) {
      const a = i >= channels ? pixels[y * stride + i - channels] : 0, b = y ? pixels[(y - 1) * stride + i] : 0;
      const c = i >= channels && y ? pixels[(y - 1) * stride + i - channels] : 0;
      const paeth = () => { const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c); return pa <= pb && pa <= pc ? a : pb <= pc ? b : c; };
      pixels[y * stride + i] = (line[i] + [0, a, b, (a + b) >> 1, paeth()][filter]) & 0xff;
    }
  }
  return {width, height, at: (x, y) => [0, 1, 2].map(k => pixels[y * stride + x * channels + k])};
}
const differs = (p, q) => Math.abs(p[0] - q[0]) + Math.abs(p[1] - q[1]) + Math.abs(p[2] - q[2]) > 24;
// Rows in [y0, y1) where most pixels of columns [x0, x1) differ from the baseline.
function changedRows(frame, base, x0, x1, y0, y1) {
  const rows = [];
  for (let y = Math.max(0, y0); y < Math.min(frame.height, y1); y++) {
    let changed = 0;
    for (let x = x0; x < x1; x++) if (differs(frame.at(x, y), base.at(x, y))) changed++;
    if (changed * 2 > x1 - x0) rows.push(y);
  }
  return rows;
}

const page = webkit ? await playwrightWebkit() : await chrome();
const {evaluate} = page;
for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
// The note's blocks in order: text, bounds and handle, in page pixels.
const blocks = () => evaluate(`(async () => {
  const stale = tiloMount.state().semantics;
  tiloMount.requestSemantics();
  for (let i = 0; i < 60 && tiloMount.state().semantics === stale; i++) await new Promise(done => setTimeout(done, 50));
  const canvas = tiloMount.canvas.getBoundingClientRect(), found = [];
  const page = b => b && {x0: b.x0 + canvas.left, y0: b.y0 + canvas.top, x1: b.x1 + canvas.left, y1: b.y1 + canvas.top};
  const walk = node => {
    if (!node || typeof node !== 'object') return;
    if (/^notes\\.block:/.test(node.semantic_key ?? '')) {
      const kids = node.children ?? [];
      found.push({text: kids.find(k => /^notes\\.block-text:/.test(k.semantic_key ?? ''))?.value ?? '',
        bounds: page(node.bounds), handle: page(kids.find(k => /^notes\\.block-type:/.test(k.semantic_key ?? ''))?.bounds)});
      return;
    }
    for (const child of node.children ?? []) walk(child);
    for (const child of node.semantics?.children ?? []) walk(child);
  };
  walk(tiloMount.state().semantics.root);
  return found;
})()`);

await command({kind: 'launch', app: 'so.tilo.notes'});
await sleep(3000);
// Start a note from the empty state's button, then write five paragraphs.
const start = await evaluate(`(async () => {
  tiloMount.requestSemantics();
  await new Promise(done => setTimeout(done, 300));
  let hit;
  const walk = (node, rect) => {
    if (!node || hit) return;
    const access = node.semantics ?? node, bounds = access.bounds ?? (rect && {x0: rect[0], y0: rect[1], x1: rect[0] + rect[2], y1: rect[1] + rect[3]});
    if (access.role === 'Button' && access.name === 'New note' && bounds) { hit = bounds; return; }
    for (const child of node.children ?? []) walk(child, child.rect);
    for (const child of node.semantics?.children ?? []) walk(child);
  };
  walk(tiloMount.state()?.semantics?.root);
  return hit;
})()`);
assert.ok(start, 'the empty Notes window offers New note');
const [sx, sy] = [(start.x0 + start.x1) / 2, (start.y0 + start.y1) / 2];
const click = async (x, y) => { await page.move(x, y); await page.down(x, y); await page.up(x, y); };
await click(sx, sy);
await sleep(1500);
const body = await evaluate(`(() => {
  let rect;
  const walk = node => { if (!node || rect) return; if (node.widget === 'BlockEditor') rect = node.rect; for (const c of node.children ?? []) walk(c); };
  walk(tiloMount.state()?.semantics?.root);
  const canvas = tiloMount.canvas.getBoundingClientRect();
  return rect && {x: rect[0] + canvas.left + 40, y: rect[1] + canvas.top + 12};
})()`);
assert.ok(body, 'the new note shows its body');
await click(body.x, body.y);
await sleep(500);
const TEXTS = ['Alpha first paragraph', 'Bravo second paragraph', 'Charlie third paragraph', 'Delta fourth paragraph', 'Echo fifth paragraph'];
for (const [i, text] of TEXTS.entries()) {
  await page.type(text);
  if (i < TEXTS.length - 1) await page.enter();
}
await sleep(800);
let order = await blocks();
assert.deepEqual(order.map(b => b.text), TEXTS, 'the note holds the five paragraphs');
const [first, , , fourth] = order;
assert.ok(first.handle, 'the first block exposes its handle');

const hx = Math.round((first.handle.x0 + first.handle.x1) / 2), hy = Math.round((first.handle.y0 + first.handle.y1) / 2);
await page.move(hx, hy);
await sleep(400);
const base = decodePng(await page.shot());
const frames = () => evaluate('tiloMount.state()?.frames ?? 0');
// Beyond the short paragraphs' text: only the drop indicator changes there.
const indicatorColumns = [Math.round(first.bounds.x1 - 160), Math.round(first.bounds.x1 - 20)];
// Between the handle and the text: only the ghost card changes there.
const ghostColumns = [Math.ceil(first.handle.x1) + 2, Math.floor(first.bounds.x0) + 2];
const rowsTop = Math.floor(first.bounds.y0 - 40), rowsBottom = Math.ceil(order.at(-1).bounds.y1 + 60);
const STEP = 3, target = Math.round((fourth.bounds.y0 + fourth.bounds.y1) / 2);
await page.down(hx, hy);
await sleep(60);
const trace = [];
const framesBefore = await frames();
for (let y = hy + STEP; y <= target; y += STEP) {
  await page.move(hx, y);
  await sleep(16);
  const frame = decodePng(await page.shot());
  const ghost = changedRows(frame, base, ...ghostColumns, rowsTop, rowsBottom);
  const indicator = changedRows(frame, base, ...indicatorColumns, rowsTop, rowsBottom);
  trace.push({dy: y - hy, ghostTop: ghost[0] ?? null, indicator: indicator.length ? indicator[Math.floor(indicator.length / 2)] : null, frames: await frames()});
}
const framesDuring = trace.at(-1).frames - framesBefore;
await page.up(hx, target);
await sleep(800);
order = await blocks();
console.log(JSON.stringify({step: 'trace', engine: webkit ? 'webkit' : 'chrome', trace}));

// The drag arms after a few pixels; from then on every step shows it.
const live = trace.filter(row => row.dy >= 6);
assert.deepEqual(live.filter(row => row.ghostTop == null), [], 'the ghost shows on every step of the drag');
const ghostTop0 = live[0].ghostTop - live[0].dy;
const lag = live.map(row => Math.abs(row.ghostTop - row.dy - ghostTop0));
assert.ok(Math.max(...lag) <= 2, `the ghost follows the pointer 1:1: ${JSON.stringify(lag)}`);
// Over its own block the drag has nowhere to go yet; once the indicator
// shows it stays until the release.
const shown = live.findIndex(row => row.indicator != null);
assert.ok(shown >= 0, 'the drop indicator shows');
assert.deepEqual(live.slice(shown).filter(row => row.indicator == null), [], 'the drop indicator never drops out mid-drag');
const moves = live.slice(shown).map(row => row.indicator);
assert.ok(moves.every((y, i) => i === 0 || y >= moves[i - 1] - 1), `the drop indicator only moves forward: ${JSON.stringify(moves)}`);
const positions = [...new Set(moves.map(y => Math.round(y / 4)))];
assert.ok(positions.length <= order.length, `the indicator visits each boundary at most once: ${JSON.stringify(moves)}`);
console.log(JSON.stringify({step: 'drag', steps: trace.length, indicatorPositions: positions.length, presentedFrames: framesDuring,
  presentedPerMove: +(framesDuring / trace.length).toFixed(2), maxGhostLag: Math.max(...lag), ok: true}));

assert.deepEqual(order.map(b => b.text), [TEXTS[1], TEXTS[2], TEXTS[3], TEXTS[0], TEXTS[4]], 'the release moves the block where the indicator was');
console.log(JSON.stringify({step: 'drop', order: order.map(b => b.text.split(' ')[0]), ok: true}));
assert.deepEqual(page.errors(), [], 'no page exception');
await page.close();
await server.close();
