// A person putting a folder into their workspace from the page of the local
// tilo.sh, in their own headless Chromium: a fresh profile opens the site and
// lands in the Lounge (or, with --follow LINK, in the workspace an
// invitation names). Then the folder FOLDER goes up twice:
//
//   1. dropped on the desk: a drop of the folder's real files, carrying the
//      folder itself as a person's file manager hands it over, onto the desk
//      at a point no window covers;
//   2. chosen in the desk's menu: "Upload a folder…" from the desk's search,
//      and the folder given to the browser's own chooser.
//
// Each time it waits for the upload's job on the desk to end, keeps a
// picture of the desk while it runs and once it ended, and reads what the
// job says came of it.
//
// usage: node upload.mjs SITE RELAY OUT_DIR FOLDER [--follow LINK] [--timeout SECONDS]
//
// Prints one JSON line: {room, files, bytes, drop: {ms, outcome}, picker: {ms,
// outcome}}. Exits non-zero, saying what never happened, when a step did not.
import {mkdirSync, readdirSync, statSync, writeFileSync} from 'node:fs';
import {join, relative, resolve} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, out, folderArg, ...rest] = process.argv.slice(2);
const option = name => {
  const at = rest.indexOf(name);
  return at < 0 ? null : rest[at + 1];
};
if (!site || !relay || !out || !folderArg) {
  console.error('usage: node upload.mjs SITE RELAY OUT_DIR FOLDER [--follow LINK] [--timeout SECONDS]');
  process.exit(2);
}
const folder = resolve(folderArg);
const follow = option('--follow');
const timeoutMs = Number(option('--timeout') ?? 900) * 1000;
mkdirSync(out, {recursive: true});

// Every file under the folder, with its path from the folder's own place
// (`docs/2026/plan.md` for a folder `docs`), as the page names them.
const walk = dir => readdirSync(dir, {withFileTypes: true}).flatMap(entry => {
  const path = join(dir, entry.name);
  return entry.isDirectory() ? walk(path) : entry.isFile() ? [path] : [];
});
const files = walk(folder).sort();
const base = resolve(folder, '..');
const named = files.map(path => relative(base, path));
const report = {site, relay, folder, files: files.length, bytes: files.reduce((sum, path) => sum + statSync(path).size, 0)};

// The stack is plain HTTP on this machine: no certificate to accept, as for
// any person's browser. The GPU drawing path on a software device, and no
// sandbox where this runs as root.
const browser = await launch({args: [
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false})
  .catch(() => {});

const text = () => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 600)`)
  .catch(() => '');
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

// Arrive, and be in a workspace.
await tab.navigate(site);
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
  await finish(1, `the page stopped arriving: ${await text()}`);
}
const memberships = `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')`;
report.room = JSON.parse(await until(`(() => {
  const kept = ${memberships}[${JSON.stringify(new URL(relay).origin)}];
  return kept?.room ? JSON.stringify(kept) : '';
})()`, 'joining the Lounge')).room;
if (follow) {
  await tab.navigate(follow);
  await until(`(() => {
    const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (!join || join.disabled) return false;
    join.click();
    return true;
  })()`, 'the invitation offering to join');
}
await until('Boolean(globalThis.tiloMount?.canvas)', 'the desk mounting');
// The desk offers its upload rows once the workspace says this person may
// change its records.
console.error(`ok: in the workspace; ${files.length} files (${report.bytes} bytes) to put up from ${folder}`);

// The names the desk's semantic tree shows now: the job's rows are among them.
const names = `(() => {
  const found = [];
  const walk = node => { if (!node) return; if (node.name) found.push(node.name); if (node.value) found.push(String(node.value));
    (node.children || []).forEach(walk); if (node.semantics) walk(node.semantics); };
  walk(globalThis.tiloMount?.state()?.semantics?.root);
  return found;
})()`;
// The rows titled `title` the desk shows: for each, whether it still runs (it
// offers Stop) and what its end line says.
const rowsOf = async title => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  const shown = await tab.evaluate(`JSON.stringify(${names})`).then(JSON.parse).catch(() => []);
  return shown.flatMap((said, at) => {
    if (said !== title) return [];
    const row = shown.slice(at + 1, at + 6);
    const next = row.findIndex(other => other === title);
    const own = next < 0 ? row : row.slice(0, next);
    return [{running: own.includes('Stop'),
      outcome: own.find(line => /files? in |kept as is|not uploaded|Stopped|did not finish/i.test(line)) ?? null}];
  });
};
// Wait for one more upload job titled `title` than the `before` the desk
// showed to end: what its row says it came to.
const jobEnded = async (title, name, before) => {
  const began = Date.now();
  let shotRunning = false;
  for (;;) {
    const rows = await rowsOf(title);
    if (rows.some(row => row.running) && !shotRunning) {
      await shot(`${name}-running`);
      shotRunning = true;
    }
    const ended = rows.length > before && rows.every(row => !row.running)
      && rows.map(row => row.outcome).filter(Boolean).at(-1);
    if (ended) {
      await sleep(500);
      await shot(`${name}-ended`);
      return {ms: Date.now() - began, outcome: ended};
    }
    if (Date.now() - began > timeoutMs) await finish(1, `the ${name} upload never ended; the desk shows rows ${JSON.stringify(rows)}: ${await text()}`);
    await sleep(500);
  }
};
const title = files.length === 1 ? `Uploading ${named[0]}` : `Uploading ${files.length} files`;

// The folder's real files, in the page: given to a file input of the page's
// own, as the browser hands a person's files over, then read back as Files.
const doc = await tab.send('DOM.getDocument', {depth: 0});
await tab.evaluate(`(() => { const input = document.createElement('input'); input.type = 'file'; input.multiple = true;
  input.id = 'upload-check-files'; input.style.display = 'none'; document.body.append(input); return true; })()`);
const holder = await tab.send('DOM.querySelector', {nodeId: doc.result.root.nodeId, selector: '#upload-check-files'});
const set = await tab.send('DOM.setFileInputFiles', {files, nodeId: holder.result.nodeId});
if (set.error) await finish(1, `the browser took none of the folder's files: ${set.error.message}`);

const droppedBefore = (await rowsOf(title)).length;
// 1. The drop: the folder, as a file manager drops it, onto the desk where no
// window stands (the desktop says where windows are).
const dropped = await tab.evaluate(`(() => {
  const files = [...document.getElementById('upload-check-files').files];
  const paths = ${JSON.stringify(named)};
  // A folder entry tree over the real files, as a browser's file system
  // entries answer a dropped folder.
  const folder = name => ({name, isFile: false, isDirectory: true, children: new Map(),
    createReader() { let given = false; return {readEntries: done => { const all = given ? [] : [...this.children.values()]; given = true; setTimeout(() => done(all)); }}; }});
  const top = new Map();
  files.forEach((file, index) => {
    const parts = paths[index].split('/');
    let at = top;
    for (const part of parts.slice(0, -1)) {
      if (!at.has(part)) at.set(part, folder(part));
      at = at.get(part).children;
    }
    at.set(file.name, {name: file.name, isFile: true, isDirectory: false, file: done => setTimeout(() => done(file))});
  });
  const items = [...top.values()].map(entry => ({kind: 'file', webkitGetAsEntry: () => entry, getAsFile: () => null}));
  const canvas = globalThis.tiloMount.canvas;
  const rect = canvas.getBoundingClientRect();
  // The desk's background: the strip's foot left of centre, below the windows' bars.
  const clientX = rect.left + rect.width * 0.25, clientY = rect.top + rect.height * 0.85;
  const data = {types: ['Files'], items, files: [], dropEffect: 'none'};
  for (const type of ['dragenter', 'dragover', 'drop']) {
    const event = new Event(type, {bubbles: true, cancelable: true});
    Object.defineProperties(event, {dataTransfer: {value: data}, clientX: {value: clientX}, clientY: {value: clientY}});
    canvas.dispatchEvent(event);
  }
  return JSON.stringify({clientX, clientY, files: files.length});
})()`).then(JSON.parse);
console.error(`ok: dropped ${dropped.files} files at (${dropped.clientX}, ${dropped.clientY})`);
report.drop = await jobEnded(title, 'drop', droppedBefore);
console.error(`ok: the drop's upload ended in ${report.drop.ms} ms: ${report.drop.outcome}`);

// 2. The desk's menu: "Upload a folder…", found in the desk's search, and the
// folder given to the chooser it opens.
const pickedBefore = (await rowsOf(title)).length;
await tab.send('Page.setInterceptFileChooserDialog', {enabled: true}).catch(() => {});
const key = async (key, code, modifiers = 0, text = '') => {
  await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key, code, modifiers, text});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key, code, modifiers});
};
await key('k', 'KeyK', 2);
await sleep(800);
for (const char of 'Upload a folder') await tab.send('Input.insertText', {text: char});
await sleep(800);
await key('Enter', 'Enter', 0, '\r');
await until(`Boolean(document.querySelector('input[data-upload="folder"]'))`, 'the desk menu opening the folder chooser', 30_000);
const fresh = await tab.send('DOM.getDocument', {depth: 0});
const chooser = await tab.send('DOM.querySelector', {nodeId: fresh.result.root.nodeId, selector: 'input[data-upload="folder"]'});
const chose = await tab.send('DOM.setFileInputFiles', {files: [folder], nodeId: chooser.result.nodeId});
if (chose.error) await finish(1, `the chooser took no folder: ${chose.error.message}`);
report.picker = await jobEnded(title, 'picker', pickedBefore);
console.error(`ok: the chosen folder's upload ended in ${report.picker.ms} ms: ${report.picker.outcome}`);

await finish(0);
