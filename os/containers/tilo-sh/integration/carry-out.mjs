// A person getting the workspace's files out of the page of the local
// tilo.sh, in their own headless Chromium: a fresh profile lands in the
// Lounge, opens Files, and takes out files the workspace's tree already holds
// (`tilo files put` put them there):
//
//   1. dragged out: a row of Files pressed and dragged, as a person drags it
//      towards their desktop. The browser's drag is intercepted where it
//      starts (Chrome saves a `DownloadURL` drag where it lands only on
//      Windows and macOS, and a headless browser has no desktop to drop on),
//      so the check reads the `DownloadURL` the drag carries and fetches it
//      the way the browser's download manager does on such a drop: a
//      download of that address, outside the page, into DOWNLOADS. The same
//      drag let go over the page stays the window's own drag.
//   2. saved: the row's Download, once into a Save dialog's file (the dialog
//      answered with a file that counts what is written, as the picker's
//      writable takes it) and once as the browser's own download. The
//      renderer's memory is sampled all the while.
//
// usage: node carry-out.mjs SITE RELAY OUT_DIR DOWNLOADS FILE... [--folder NAME] [--save NAME] [--follow LINK]
//
// Each FILE is a local file whose name is the name of a file at the root of
// the workspace's tree, with the same bytes; --folder names a folder at the
// root to drag out as a zip; --save names the FILE to save (default: the
// largest); --follow is an invitation into a workspace that takes people by
// invitation, which the page follows first. Prints one JSON line of what happened and what it measured.
import {createHash} from 'node:crypto';
import {createReadStream, existsSync, mkdirSync, openSync, readFileSync, readSync, closeSync, readdirSync, statSync, writeFileSync} from 'node:fs';
import {basename, join, resolve} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const argv = process.argv.slice(2);
const option = name => {
  const at = argv.indexOf(name);
  if (at < 0) return null;
  const [value] = argv.splice(at, 2).slice(1);
  return value;
};
const folderName = option('--folder');
const saveName = option('--save');
const follow = option('--follow');
const [site, relay, out, downloadsArg, ...fileArgs] = argv;
if (!site || !relay || !out || !downloadsArg || !fileArgs.length) {
  console.error('usage: node carry-out.mjs SITE RELAY OUT_DIR DOWNLOADS FILE... [--folder NAME] [--save NAME] [--follow LINK]');
  process.exit(2);
}
const downloads = resolve(downloadsArg);
mkdirSync(out, {recursive: true});
mkdirSync(downloads, {recursive: true});
const files = fileArgs.map(path => ({path: resolve(path), name: basename(path), size: statSync(path).size}));
const saving = files.find(file => file.name === saveName) ?? [...files].sort((a, b) => b.size - a.size)[0];
const report = {site, relay, files: files.map(({name, size}) => ({name, size}))};

const sha256 = path => new Promise((done, fail) => {
  const hash = createHash('sha256');
  createReadStream(path).on('data', chunk => hash.update(chunk)).on('end', () => done(hash.digest('hex'))).on('error', fail);
});

const browser = await launch({args: ['--disable-dev-shm-usage', ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
await browser.send('Browser.setDownloadBehavior', {behavior: 'allow', downloadPath: downloads, eventsEnabled: true});

const text = () => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 600)`).catch(() => '');
const shot = async name => {
  const picture = await tab.screenshot().catch(() => null);
  if (picture) writeFileSync(join(out, `${name}.png`), picture);
};
const finish = async (code, message) => {
  if (message) report.error = message;
  writeFileSync(join(out, 'console.log'), tab.log.join('\n'));
  await shot('page');
  await browser.close().catch(() => {});
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const until = async (expression, what, timeout = 180_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await finish(1, `${what} never happened: ${await text()}`);
    await sleep(250);
  }
};
const names = `(() => {
  const found = [];
  const walk = node => { if (!node) return; if (node.name) found.push(node.name); if (node.value) found.push(String(node.value));
    (node.children || []).forEach(walk); if (node.semantics) walk(node.semantics); };
  walk(globalThis.tiloMount?.state()?.semantics?.root);
  return found;
})()`;
const key = async (key, code, modifiers = 0, text = '') => {
  await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key, code, modifiers, text});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key, code, modifiers});
};
const mouse = (type, x, y, extra = {}) => tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', ...extra});

// Arrive in the workspace: by the invitation where one is given, else as a
// visitor of the site's Lounge.
await tab.navigate(follow ?? site);
if (follow) await until(`(() => {
  const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
  if (!join || join.disabled) return false;
  join.click();
  return true;
})()`, 'the invitation offering to join');
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') await finish(1, `the page stopped arriving: ${await text()}`);
await until('Boolean(globalThis.tiloMount?.canvas)', 'the desk mounting');
console.error('ok: on the desk');

// Open Files by its link, as following a link to an app opens it: the
// workspace the page joined, and the app in it.
const workspace = await until(`(() => {
  const kept = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(new URL(relay).origin)}];
  return kept?.workspace ?? '';
})()`, 'joining the workspace');
report.workspace = workspace;
await sleep(1500);
// The address names the workspace the way the page wrote it (its host, its
// name); the app goes after it.
const shown = await tab.evaluate('location.href');
const base = shown.includes('#') ? shown.replace(/[?].*$/, '').replace(/\/so\.tilo\.[^/]*.*$/, '') : `${shown}#/w/${workspace}`;
console.error(`ok: the page shows ${shown}`);
await tab.evaluate(`(() => { location.href = ${JSON.stringify(`${base}/so.tilo.file-explorer`)}; return true; })()`);
// The rows that carry files out are the mirror's draggable elements, named by the file.
const rowOf = name => `(() => {
  const row = [...document.querySelectorAll('[data-tilo-carries]')].find(element => element.getAttribute('aria-label') === ${JSON.stringify(name)});
  if (!row) return null;
  const box = row.getBoundingClientRect();
  if (box.width < 2 || box.height < 2) return null;
  return JSON.stringify({x: box.left + Math.min(box.width / 3, 120), y: box.top + box.height / 2, draggable: row.draggable,
    pointer: getComputedStyle(row).pointerEvents});
})()`;
const wanted = [...files.map(file => file.name), ...(folderName ? [folderName] : [])];
const rows = {};
for (const name of wanted) {
  rows[name] = JSON.parse(await until(`(() => { globalThis.tiloMount?.requestSemantics?.(); return ${rowOf(name)}; })()`, `Files showing ${name} as a row that drags out`, 120_000));
}
report.rows = rows;
await shot('files');
console.error(`ok: Files shows ${wanted.join(', ')}, each draggable`);

// What reaches the canvas while a drag is under way.
await tab.evaluate(`(() => { globalThis.__carrySeen = []; for (const type of ['pointerdown', 'pointermove', 'pointerup', 'pointercancel'])
  globalThis.tiloMount.canvas.addEventListener(type, event => globalThis.__carrySeen.push([type, Math.round(event.clientX), Math.round(event.clientY)]), {capture: true}); return true; })()`);

// 1. Drag out: hover (the page asks for the link), press, travel.
await tab.send('Input.setInterceptDrags', {enabled: true});
const dragOut = async name => {
  const {x, y} = rows[name];
  const intercepted = new Promise(done => {
    const off = browser.on(message => {
      if (message.method !== 'Input.dragIntercepted' || message.sessionId !== tab.sessionId) return;
      off();
      done(message.params.data);
    });
  });
  await mouse('mouseMoved', x, y, {button: 'none'});
  await sleep(1200);
  const pressedAt = performance.now();
  await mouse('mousePressed', x, y, {clickCount: 1, buttons: 1});
  for (let step = 1; step <= 6; step++) await mouse('mouseMoved', x + step * 8, y + step * 3, {buttons: 1});
  const data = await Promise.race([intercepted, sleep(5000).then(() => null)]);
  return {data, ms: Math.round(performance.now() - pressedAt), at: {x: x + 48, y: y + 18}};
};
const carriedUrl = data => {
  const item = data?.items?.find(item => item.mimeType === 'DownloadURL');
  if (!item) return null;
  const [mime, name, ...rest] = item.data.split(':');
  return {mime, name, url: rest.join(':')};
};
// The browser's download manager fetching the address a drop names: a
// download of it, outside the page, into the download directory.
const downloaded = async (url, name, timeout = 600_000) => {
  const before = new Set(readdirSync(downloads));
  const fetcher = await browser.tab();
  const began = Date.now();
  await fetcher.navigate(url);
  const target = join(downloads, name);
  for (;;) {
    const partial = readdirSync(downloads).some(entry => entry.endsWith('.crdownload'));
    if (!partial && existsSync(target) && !before.has(name)) break;
    if (Date.now() - began > timeout) await finish(1, `${name} never landed in ${downloads}`);
    await sleep(200);
  }
  await fetcher.close().catch(() => {});
  return {path: target, ms: Date.now() - began};
};

report.drags = [];
const small = [...files].sort((a, b) => a.size - b.size)[0];
for (const name of [small.name, ...(folderName ? [folderName] : [])]) {
  const drag = await dragOut(name);
  const carried = carriedUrl(drag.data);
  if (!carried) await finish(1, `dragging ${name} carried no DownloadURL: ${JSON.stringify(drag.data)}`);
  // Let go over the page: the window's drag, not a file's.
  await tab.send('Input.dispatchDragEvent', {type: 'dragEnter', x: drag.at.x, y: drag.at.y, data: drag.data});
  await tab.send('Input.dispatchDragEvent', {type: 'dragOver', x: drag.at.x + 30, y: drag.at.y + 10, data: drag.data});
  await tab.send('Input.dispatchDragEvent', {type: 'drop', x: drag.at.x + 30, y: drag.at.y + 10, data: drag.data});
  await mouse('mouseReleased', drag.at.x + 30, drag.at.y + 10, {clickCount: 1});
  await sleep(300);
  const seen = await tab.evaluate('JSON.stringify(globalThis.__carrySeen.splice(0))').then(JSON.parse);
  const landed = await downloaded(carried.url, carried.name);
  const entry = {name, carried: {mime: carried.mime, name: carried.name, host: new URL(carried.url).origin}, startMs: drag.ms,
    canvas: seen.map(([type]) => type).filter((type, at, all) => type !== all[at - 1]), downloadMs: landed.ms, bytes: statSync(landed.path).size};
  if (name === folderName) {
    entry.zip = JSON.parse(await new Promise(done => {
      // A file manager's reading of the zip.
      import('node:child_process').then(({execFileSync}) => done(execFileSync('python3', ['-I', '-c',
        'import json,sys,zipfile;z=zipfile.ZipFile(sys.argv[1]);bad=z.testzip();print(json.dumps({"members":[(i.filename,i.file_size) for i in z.infolist()],"intact":bad is None}))', landed.path]).toString()));
    }));
  } else {
    const source = files.find(file => file.name === name);
    entry.same = (await sha256(landed.path)) === (await sha256(source.path));
  }
  report.drags.push(entry);
  console.error(`ok: dragging ${name} carried ${carried.mime}:${carried.name}; over the page the canvas saw ${entry.canvas.join(' → ')}; fetched as a drop would be, it landed in ${landed.ms} ms`);
}
await tab.send('Input.setInterceptDrags', {enabled: false});
await shot('after-drags');

// 2. Save: the renderers' memory, sampled while the file goes.
const renderers = async () => {
  const info = await browser.send('SystemInfo.getProcessInfo');
  return (info.result?.processInfo ?? []).filter(process => process.type === 'renderer').map(process => process.id);
};
const rss = pid => {
  try {
    const line = readFileSync(`/proc/${pid}/status`, 'utf8').split('\n').find(line => line.startsWith('VmRSS:'));
    return Number(line.split(/\s+/)[1]) * 1024;
  } catch { return 0; }
};
const sampled = () => {
  let peak = 0;
  let pids = [];
  let stopped = false;
  const sample = async () => {
    while (!stopped) {
      pids = await renderers().catch(() => pids);
      peak = Math.max(peak, pids.reduce((sum, pid) => sum + rss(pid), 0));
      await sleep(250);
    }
  };
  const running = sample();
  return {stop: async () => { stopped = true; await running; return peak; }};
};
const heap = async () => (await tab.send('Performance.getMetrics')).result?.metrics?.find(metric => metric.name === 'JSHeapUsedSize')?.value ?? null;
await tab.send('Performance.enable', {});
const downloadButton = name => `(() => {
  const row = [...document.querySelectorAll('[data-tilo-carries]')].find(element => element.getAttribute('aria-label') === ${JSON.stringify(name)});
  const button = row && [...row.querySelectorAll('[role=button],button')].find(element => /^Download/.test(element.getAttribute('aria-label') ?? ''));
  if (!button) return null;
  const box = button.getBoundingClientRect();
  return JSON.stringify({x: box.left + box.width / 2, y: box.top + box.height / 2});
})()`;
const savedJob = async (name, timeout = 900_000) => {
  const began = Date.now();
  for (;;) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    const shown = await tab.evaluate(`JSON.stringify(${names})`).then(JSON.parse).catch(() => []);
    const done = shown.find(line => line === `Saved ${name}` || line.startsWith(`${name} was not saved`));
    if (done) return done;
    if (Date.now() - began > timeout) await finish(1, `saving ${name} never ended`);
    await sleep(500);
  }
};
const baseline = (await renderers()).reduce((sum, pid) => sum + rss(pid), 0);
const heapBefore = await heap();
report.save = {name: saving.name, size: saving.size, baselineRss: baseline, heapBefore};

// a. Into the file a Save dialog chose: the page pipes the link's response
// into the file's writable. The dialog is answered with a file that counts
// what is written and keeps a byte at each of a few places to compare.
const probes = [0, 1, 4096, Math.floor(saving.size / 3), Math.floor(saving.size / 2), saving.size - 1].filter(at => at >= 0 && at < saving.size);
await tab.evaluate(`(() => {
  const probes = ${JSON.stringify(probes)};
  globalThis.__carrySaved = {written: 0, bytes: {}, closed: false};
  globalThis.showSaveFilePicker = async ({suggestedName}) => ({name: suggestedName, async createWritable() {
    const saved = globalThis.__carrySaved;
    return new WritableStream({
      write(chunk) {
        const view = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk.buffer ?? chunk);
        for (const at of probes) if (at >= saved.written && at < saved.written + view.byteLength) saved.bytes[at] = view[at - saved.written];
        saved.written += view.byteLength;
      },
      close() { saved.closed = true; },
    });
  }});
  return true;
})()`);
const button = JSON.parse(await until(downloadButton(saving.name), `the Download of ${saving.name}`, 30_000));
let memory = sampled();
let began = Date.now();
await mouse('mousePressed', button.x, button.y, {clickCount: 1, buttons: 1});
await mouse('mouseReleased', button.x, button.y, {clickCount: 1});
const pickerOutcome = await savedJob(saving.name);
const pickerMs = Date.now() - began;
const pickerPeak = await memory.stop();
const saved = await tab.evaluate('JSON.stringify(globalThis.__carrySaved)').then(JSON.parse);
const handle = openSync(saving.path, 'r');
const expected = Object.fromEntries(probes.map(at => { const one = Buffer.alloc(1); readSync(handle, one, 0, 1, at); return [at, one[0]]; }));
closeSync(handle);
report.save.picker = {outcome: pickerOutcome, ms: pickerMs, written: saved.written, closed: saved.closed,
  probesMatch: probes.every(at => saved.bytes[at] === expected[at]), peakRss: pickerPeak, rssGrowth: pickerPeak - baseline, heapAfter: await heap()};
console.error(`ok: saved ${saving.name} into the chosen file in ${pickerMs} ms (${saved.written} bytes); renderers peaked at ${(pickerPeak / 1048576).toFixed(0)} MiB from ${(baseline / 1048576).toFixed(0)} MiB`);

// b. As the browser's own download, where there is no Save dialog.
await tab.evaluate('(() => { delete globalThis.showSaveFilePicker; globalThis.showSaveFilePicker = undefined; return true; })()');
const before = new Set(readdirSync(downloads));
memory = sampled();
began = Date.now();
await mouse('mousePressed', button.x, button.y, {clickCount: 1, buttons: 1});
await mouse('mouseReleased', button.x, button.y, {clickCount: 1});
await savedJob(saving.name);
const target = join(downloads, saving.name);
for (;;) {
  const partial = readdirSync(downloads).some(entry => entry.endsWith('.crdownload'));
  if (!partial && existsSync(target) && !before.has(saving.name)) break;
  if (Date.now() - began > 900_000) await finish(1, `the download of ${saving.name} never landed`);
  await sleep(250);
}
const downloadMs = Date.now() - began;
const downloadPeak = await memory.stop();
report.save.download = {ms: downloadMs, bytes: statSync(target).size, same: (await sha256(target)) === (await sha256(saving.path)),
  peakRss: downloadPeak, rssGrowth: downloadPeak - baseline};
console.error(`ok: the browser downloaded ${saving.name} in ${downloadMs} ms; renderers peaked at ${(downloadPeak / 1048576).toFixed(0)} MiB`);
await finish(0);
