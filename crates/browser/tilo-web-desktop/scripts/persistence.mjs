// Content made in each web app survives a reload: headless Chrome mounts an
// app from a built desktop bundle at 1280x800, makes content the way a user
// does (pointer, keys, the file chooser), waits until the page reports it
// committed, reloads and looks for the content in the app's accessibility
// tree.
//
// usage: node persistence.mjs BUNDLE_DIR [APP_ID...]
//   BUNDLE_DIR  build.sh --bundle desktop output
// Writes a before/after screenshot per app to SHOTS_DIR when set. Set SERVER
// to a running `serve.mjs --fetch-proxy` origin for apps that read the network
// (weather, maps, tube); otherwise the bundle is served without the proxy.
import {readFileSync, writeFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {deflateSync} from 'node:zlib';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';
import {fixtureBytes} from '../../../../tools/web/fixture-assets.mjs';

const [bundle, ...only] = process.argv.slice(2);
const shots = process.env.SHOTS_DIR;
const nonce = `P${Date.now().toString(36).slice(-5)}`;
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');

// Files the page's chooser answers with, by fixture name.
const FIXTURES = {
  bin: () => file('persist-probe.bin', 'application/octet-stream', Buffer.from('PERSIST-HEX-0123456789')),
  left: () => file('left.txt', 'text/plain', Buffer.from('alpha\nPERSIST-LEFT\ngamma\n')),
  right: () => file('right.txt', 'text/plain', Buffer.from('alpha\nPERSIST-RIGHT\ngamma\n')),
  png: () => file('persist-probe.png', 'image/png', png(64, 48)),
  epub: () => file('sample.epub', 'application/epub+zip', fixtureBytes(repo, 'apps/ebook/assets/artifacts.json', 'sample.epub')),
  pdf: () => file('latin.pdf', 'application/pdf', fixtureBytes(repo, 'crates/platform/tilo-pdf/tests/fixtures/artifacts.json', 'latin.pdf')),
  mp4: () => file('persist-clip.mp4', 'video/mp4', fixtureBytes(repo, 'apps/video-player/tests/fixtures/artifacts.json', 'h264-aac.mp4')),
};

// Steps: {at: [x, y]} clicks; {press: 'Name'} clicks the control with that
// accessible name (`role` narrows it, `last` takes the last match); {field:
// 'Name', fill: 'text'} sets a text field through its DOM mirror; {type:
// 'text'} types; {key: 'Enter', mods} presses a key; `wait` is the pause after
// the step. `{nonce}` is replaced; `{tail}` in `expect` is the nonce without
// its first key, which a field that takes focus on that key may not receive.
// `files` lists, per chooser opening, the fixtures it answers with. `after`
// brings the content into view, before the reload and again after it
// (`viewAfterReloadOnly`: only after). Both times the tree must contain
// `expect` and must not contain `absent`.
// `blocked` names why an app cannot make content in a browser yet.
const RECIPES = {
  'so.tilo.api-tester': {steps: [{at: [775, 27]}, {type: 'https://example.com/{nonce}'}, {at: [712, 86], wait: 1200}], expect: '{tail}'},
  'so.tilo.calculator': {steps: [{at: [640, 120]}, {type: '12+30'}, {key: 'Enter'}], expect: '12+30 equals 42'},
  'so.tilo.calendar': {steps: [{press: 'New event', wait: 1200}, {press: 'Edit event title', wait: 800}, {type: '{nonce}'}, {key: 'Enter'}], expect: '{tail}'},
  'so.tilo.camera': {steps: [{wait: 2000}, {press: 'Take photo', role: 'Button', wait: 3000}], expect: 'Open latest capture in Photos'},
  'so.tilo.chat': {steps: [{field: 'Message composer', type: 'hello {nonce}'}, {key: 'Enter', wait: 1200}], expect: '{tail}'},
  'so.tilo.clocks': {steps: [{at: [710, 499]}], absent: 'No cities yet'},
  'so.tilo.contacts': {steps: [{at: [639, 501]}, {at: [656, 233]}, {type: '{nonce}'}, {at: [1215, 168]}], expect: '{tail}'},
  'so.tilo.diffmerge': {files: [['left'], ['right']], steps: [{press: 'Open two files', wait: 2500}], expect: 'left.txt and right.txt'},
  'so.tilo.ebook': {files: [['epub']], steps: [{press: 'Open book', wait: 3000}], expect: 'The Tide-House'},
  'so.tilo.feeds': {network: true, steps: [{press: 'Add subscription', wait: 1000}, {type: 'https://xkcd.com/atom.xml'}, {key: 'Enter', wait: 8000}], expect: 'xkcd'},
  'so.tilo.hex-editor': {files: [['bin']], steps: [{press: 'Open file', wait: 1500}], expect: 'persist-probe.bin; 22 bytes'},
  'so.tilo.maps': {network: true, steps: [{press: 'Search places'}, {type: 'Berlin', wait: 3000}, {key: 'Enter', wait: 5000}], expect: 'Search again for'},
  'so.tilo.music': {steps: [{at: [207, 445], wait: 1500}, {type: 'PL{nonce}'}, {at: [779, 452], wait: 1500}], expect: '{nonce}'},
  'so.tilo.notes': {steps: [{at: [759, 478]}, {type: '{nonce}'}], expect: '{nonce}'},
  'so.tilo.passwords': {steps: [{press: 'New item', wait: 1200}, {field: 'Name', fill: '{nonce}'}, {field: 'Password', fill: 'hunter2hunter2'}, {press: 'Save', role: 'Button', wait: 1500}], expect: '{nonce}'},
  'so.tilo.pdf-viewer': {files: [['pdf']], steps: [{at: [640, 492], wait: 8000}], expect: 'latin'},
  'so.tilo.photos': {files: [['png']], steps: [{press: 'Open', role: 'Button', wait: 3000}], expect: 'persist-probe.png'},
  'so.tilo.scratchpad': {steps: [{at: [625, 375]}, {type: 'note {nonce}'}], expect: '{nonce}'},
  'so.tilo.sheets': {steps: [{at: [110, 125]}, {type: '{nonce}'}, {key: 'Enter'}], expect: '{nonce}'},
  'so.tilo.slides': {steps: [{at: [1116, 681]}, {type: '{nonce}'}, {key: 'Escape'}], expect: '{nonce}'},
  'so.tilo.snippets': {steps: [{at: [401, 341]}, {at: [875, 191]}, {type: '{nonce}'}], expect: '{tail}'},
  'so.tilo.speed-test': {blocked: 'keeps no content: every run measures the network live'},
  'so.tilo.themes': {steps: [{at: [980, 715]}], expect: 'Tilo Dark copy'},
  'so.tilo.totp': {steps: [{press: 'Add account'}, {field: 'Issuer', type: '{nonce}'}, {field: 'Account name', type: 'me'}, {field: 'Secret key', type: 'JBSWY3DPEHPK3PXP'}, {press: 'Add account', role: 'Button', last: true, wait: 1500}], expect: '{tail}'},
  'so.tilo.tube': {network: true, steps: [{wait: 6000}, {at: [292, 206], wait: 6000}], after: [{press: 'History', role: 'Button', wait: 2500}], expect: 'Tube — Watch history', absent: 'Nothing watched yet'},
  'so.tilo.typst-editor': {steps: [{at: [320, 400]}, {key: 'End'}, {type: ' {nonce}'}], expect: '{nonce}'},
  'so.tilo.video-editor': {files: [['mp4']], steps: [{at: [134, 246], wait: 3000}], expect: 'persist-clip.mp4'},
  // Recent lists a video once its first frame has shown; the page keeps a
  // copy for it to reopen.
  'so.tilo.video-player': {files: [['mp4']], steps: [{press: 'Open video', role: 'Button', wait: 6000}], after: [{press: 'Show queue and recent videos', role: 'Button', wait: 1500}, {press: 'Recent', wait: 1500}], viewAfterReloadOnly: true, expect: 'persist-clip.mp4'},
  'so.tilo.weather': {network: true, steps: [{press: 'Search city…'}, {type: 'Berlin', wait: 3000}, {key: 'Enter', wait: 4000}], expect: 'Berlin'},
  'so.tilo.word-processor': {steps: [{at: [625, 375]}, {type: '{nonce}'}], expect: '{nonce}'},
};

function file(name, type, bytes) {
  return {name, type, base64: Buffer.from(bytes).toString('base64')};
}

function png(width, height) {
  const crc = buffer => {
    let c = ~0;
    for (const byte of buffer) {
      c ^= byte;
      for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
    }
    return ~c >>> 0;
  };
  const chunk = (tag, data) => {
    const body = Buffer.concat([Buffer.from(tag), data]);
    const out = Buffer.alloc(body.length + 8);
    out.writeUInt32BE(data.length, 0);
    body.copy(out, 4);
    out.writeUInt32BE(crc(body), body.length + 4);
    return out;
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 6, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array(width).fill([200, 80, 40, 255]).flat())]);
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header), chunk('IDAT', deflateSync(Buffer.concat(Array(height).fill(row)))), chunk('IEND', Buffer.alloc(0))]);
}

// The page's file chooser takes the next queued answer instead of opening.
const CHOOSER = `(() => {
  const click = HTMLInputElement.prototype.click;
  HTMLInputElement.prototype.click = function () {
    const queued = globalThis.__tiloChooserFiles;
    if (this.type !== 'file' || !queued) return click.call(this);
    const transfer = new DataTransfer();
    for (const f of queued.shift() ?? []) {
      transfer.items.add(new File([Uint8Array.from(atob(f.base64), c => c.charCodeAt(0))], f.name, {type: f.type}));
    }
    this.files = transfer.files;
    setTimeout(() => this.dispatchEvent(new Event('change')), 0);
  };
})()`;

// Every node's role, name, value and bounds, from the frame's semantic snapshot.
const NODES = `(() => {
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  const out = [];
  const node = n => { out.push({role: n.role, name: n.name ?? '', value: n.value ?? '', bounds: n.bounds ?? null}); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  if (root) walk(root);
  return out;
})()`;

const server = process.env.SERVER ? {url: process.env.SERVER, close() {}} : await serve(resolve(bundle));
// Chrome's synthetic camera answers getUserMedia without a prompt.
const browser = await launch({args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream']});
const results = [];
const presented = "/frames [1-9]/.test((document.documentElement.dataset.phase ?? '') + ' · frames ' + (document.documentElement.dataset.frames ?? 0) + ' · ' + (document.getElementById('status')?.textContent ?? ''))";
const text = s => s.replaceAll('{nonce}', nonce);

async function nodes(tab) {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(600);
  return tab.evaluate(NODES);
}

async function click(tab, x, y) {
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
}

async function control(tab, name, {role, last} = {}) {
  const all = (await nodes(tab)).filter(n => !role || n.role === role);
  const pick = list => (last ? list.at(-1) : list[0]);
  const node = pick(all.filter(n => n.name === name)) ?? pick(all.filter(n => n.name.startsWith(name)));
  if (!node) throw Error(`no control named ${JSON.stringify(name)}`);
  return node;
}

const mirror = name => `[...document.querySelectorAll('[aria-label]')].find(e => e.getAttribute('aria-label') === ${JSON.stringify(name)})`;

// Click a control where it is drawn, or activate its DOM mirror when it has
// no position.
async function reach(tab, name, options) {
  const node = await control(tab, name, options);
  if (node.bounds) {
    const b = node.bounds;
    await click(tab, (b.x0 + b.x1) / 2, (b.y0 + b.y1) / 2);
  } else if (!(await tab.evaluate(`(() => { const e = ${mirror(node.name)}; if (!e) return false; e.focus(); e.click(); return true; })()`))) {
    throw Error(`control ${name} has no position or element`);
  }
}

async function perform(tab, step) {
  if (step.at) await click(tab, ...step.at);
  else if (step.press) await reach(tab, text(step.press), step);
  else if (step.field && step.fill !== undefined) {
    const node = await control(tab, step.field, step);
    const value = JSON.stringify(text(step.fill));
    await tab.evaluate(`${mirror(node.name)}?.focus()`);
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
    await sleep(500);
    const filled = await tab.evaluate(`(() => { const e = ${mirror(node.name)}; if (!e) return false; if ('value' in e) e.value = ${value}; else e.textContent = ${value}; e.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText', data: ${value}})); return true; })()`);
    if (!filled) throw Error(`field ${step.field} has no element`);
  } else if (step.field) await reach(tab, step.field, step);
  if (step.type) {
    for (const ch of text(step.type)) {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    }
  }
  if (step.key) {
    const named = {Enter: 13, End: 35, Escape: 27, Tab: 9}[step.key];
    const code = step.key.length === 1 ? `Key${step.key.toUpperCase()}` : step.key;
    const key = {key: step.key, code, windowsVirtualKeyCode: named ?? step.key.toUpperCase().charCodeAt(0), modifiers: step.mods ?? 0};
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', ...key, ...(step.key === 'Enter' ? {text: '\r'} : {})});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...key});
  }
  await sleep(step.wait ?? 600);
}

async function tree(tab) {
  return (await nodes(tab)).map(n => `${n.name} ${n.value}`).join('\n');
}

async function committed(tab) {
  await tab.evaluate('globalThis.tiloMount?.flush?.()').catch(() => {});
  let status = '';
  for (let i = 0; i < 40; i++) {
    status = await tab.evaluate("(document.documentElement.dataset.phase ?? '') + ' · frames ' + (document.documentElement.dataset.frames ?? 0) + ' · ' + (document.getElementById('status')?.textContent ?? '')");
    if (/committed/.test(status)) break;
    await sleep(250);
  }
  return status;
}

const holds = (content, recipe) => {
  const expect = recipe.expect?.replaceAll('{nonce}', nonce).replaceAll('{tail}', nonce.slice(1));
  return (!expect || content.includes(expect)) && (!recipe.absent || !content.includes(recipe.absent));
};

try {
  const apps = only.length
    ? only
    : (await (await fetch(`${server.url}/apps.json`)).json()).applications.map(app => app.id);
  for (const app of apps) {
    const recipe = RECIPES[app];
    const result = {app, persisted: false, detail: ''};
    if (!recipe || recipe.blocked) {
      result.detail = recipe?.blocked ?? 'no recipe';
      results.push(result);
      console.log(JSON.stringify(result));
      continue;
    }
    const tab = await browser.tab();
    try {
      await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
      await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: CHOOSER});
      await tab.navigate(`${server.url}/app/#/me/${app}`);
      await tab.waitFor(presented, {timeout: 300000});
      await sleep(2000);
      if (recipe.files) {
        const answers = recipe.files.map(names => names.map(name => FIXTURES[name]()));
        await tab.evaluate(`globalThis.__tiloChooserFiles = ${JSON.stringify(answers)}`);
      }
      for (const step of recipe.steps) await perform(tab, step);
      if (!recipe.viewAfterReloadOnly) for (const step of recipe.after ?? []) await perform(tab, step);
      const before = recipe.viewAfterReloadOnly || holds(await tree(tab), recipe);
      const status = await committed(tab);
      if (shots) writeFileSync(`${shots}/${app}-before.png`, await tab.screenshot());
      await tab.reload();
      await sleep(500);
      await tab.waitFor(presented, {timeout: 300000});
      await sleep(2000);
      for (const step of recipe.after ?? []) await perform(tab, step);
      const after = holds(await tree(tab), recipe);
      if (shots) writeFileSync(`${shots}/${app}-after.png`, await tab.screenshot());
      result.persisted = before && after;
      result.detail = `made: ${before}; after reload: ${after}; ${status}`;
      if (recipe.network && !process.env.SERVER) result.detail += '; served without the fetch proxy';
      const errors = tab.log.filter(l => l.startsWith('EXC') || /panic/i.test(l));
      if (errors.length) result.detail += `; ${errors[0]}`;
    } catch (error) {
      const status = await tab.evaluate("(document.documentElement.dataset.phase ?? '') + ' · frames ' + (document.documentElement.dataset.frames ?? 0) + ' · ' + (document.getElementById('status')?.textContent ?? '')").catch(() => '?');
      result.detail = `${String(error?.message ?? error).slice(0, 300)} (status: ${status}; ${tab.log.slice(0, 2).join(' | ').slice(0, 300)})`;
    } finally {
      results.push(result);
      console.log(JSON.stringify(result));
      await tab.close();
      // The next mount takes the profile's storage lease once this page lets go.
      await sleep(4000);
    }
  }
} finally {
  await browser.close();
  await server.close();
}
const checked = results.filter(r => !RECIPES[r.app]?.blocked);
const failed = checked.filter(r => !r.persisted);
console.log(`${checked.length - failed.length}/${checked.length} persisted; ${results.length - checked.length} blocked`);
process.exit(failed.length ? 1 : 0);
