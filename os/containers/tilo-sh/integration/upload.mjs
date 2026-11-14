// A person putting files and folders into their workspace from the page of
// the local tilo.sh, in their own Chromium: a fresh profile opens the site and
// lands in the Lounge (or, with --follow LINK, in the workspace an invitation
// names). Real files from FOLDER are then dragged in as a person's file
// manager drags them (the DevTools protocol's own drag, carrying the files'
// paths), each drop's place outlined while they hover:
//
//   1. onto an empty place of the strip: one file of each kind, which open
//      side by side there (a PDF, a picture, a markdown file, and one no app
//      opens);
//   2. a folder onto an empty place of the strip, which opens Files;
//   3. a file onto that Files window, which copies it into the folder Files
//      shows;
//   4. a picture onto the open Photos window, which imports it;
//   5. a picture onto the open Notes window, which takes it itself;
//   6. onto a window whose app opens no files (Calculator, opened from the
//      desk's search), which says so where it fell;
//   7. a large file no app opens onto an empty place of the strip, kept a
//      chunk at a time;
//   8. the folder again, chosen in the desk's menu ("Upload a folder…").
//
// Each keeps a picture of the hover and of the end, and reads what the page
// said at the drop's place.
//
// usage: node upload.mjs SITE RELAY OUT_DIR FOLDER [--follow LINK]
//
// FOLDER holds the files dropped: `plan.pdf`, `photo.png`, `notes.md`,
// `data.json`, `second.png`, `big.bin` (tens of megabytes) and a folder
// `docs`. Prints one JSON line: what
// each step's page said. Exits non-zero, saying what never happened, when a
// step did not.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, out, folderArg, ...rest] = process.argv.slice(2);
const option = name => {
  const at = rest.indexOf(name);
  return at < 0 ? null : rest[at + 1];
};
if (!site || !relay || !out || !folderArg) {
  console.error('usage: node upload.mjs SITE RELAY OUT_DIR FOLDER [--follow LINK]');
  process.exit(2);
}
const folder = resolve(folderArg);
const follow = option('--follow');
mkdirSync(out, {recursive: true});
const file = name => join(folder, name);
const report = {site, relay, folder, steps: {}};

const browser = await launch({args: ['--disable-dev-shm-usage', ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1600, height: 900, deviceScaleFactor: 1, mobile: false})
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

const pressJoin = `(() => {
  const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
  if (!join || join.disabled) return false;
  join.click();
  return true;
})()`;
await tab.navigate(follow ?? site);
if (follow) await until(pressJoin, 'the invitation offering to join');
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
  await finish(1, `the page stopped arriving: ${await text()}`);
}
await until('Boolean(globalThis.tiloMount?.canvas)', 'the desk mounting');
await sleep(3000);
console.error('ok: in the workspace');

// The canvas's place on the page.
const canvas = JSON.parse(await tab.evaluate(`JSON.stringify(globalThis.tiloMount.canvas.getBoundingClientRect())`));
const drag = (type, x, y, files) => tab.send('Input.dispatchDragEvent', {
  type, x, y, data: {items: [], files, dragOperationsMask: 1},
});
// What the page shows while files hover at (x, y): the outlined place's kind
// and what its note says.
const hovering = async (x, y, files) => {
  // What an earlier point said is not what this one says.
  await tab.evaluate(`document.querySelectorAll('.tilo-drop-target, .tilo-drop-note').forEach(shown => { shown.hidden = true; })`);
  await drag('dragOver', x, y, files);
  await sleep(120);
  return JSON.parse(await tab.evaluate(`JSON.stringify({
    kind: document.querySelector('.tilo-drop-target')?.hidden === false ? document.querySelector('.tilo-drop-target').dataset.kind : null,
    note: document.querySelector('.tilo-drop-note')?.hidden === false ? document.querySelector('.tilo-drop-note').textContent : null,
  })`));
};
// A point of the desk where hovering `files` says `wanted` (a note's words, or
// a place's kind): scanned across the desk.
const find = async (files, wanted) => {
  await drag('dragEnter', canvas.left + 10, canvas.top + 10, files);
  for (let y = 0.3; y <= 0.9; y += 0.15) {
    for (let x = 0.03; x <= 0.97; x += 0.03) {
      const at = [canvas.left + canvas.width * x, canvas.top + canvas.height * y];
      const seen = await hovering(...at, files);
      if (wanted(seen)) return {at, seen};
    }
  }
  // The gaps between a full strip's windows are a few pixels wide.
  for (let x = canvas.left + 2; x < canvas.left + canvas.width; x += 3) {
    const at = [x, canvas.top + canvas.height * 0.6];
    const seen = await hovering(...at, files);
    if (wanted(seen)) return {at, seen};
  }
  return null;
};
// What the page showed of earlier drops, marked: a finished one leaves on its own.
const markShown = () => tab.evaluate(`document.querySelectorAll('.tilo-drop-progress').forEach(box => { box.dataset.seen = ''; })`);
// Drop `files` at a point `wanted` finds, keeping pictures of the hover and
// of what the page says at the drop's end.
const dropStep = async (name, files, wanted) => {
  const found = await find(files, wanted);
  if (!found) {
    await drag('dragCancel', 0, 0, files).catch(() => {});
    await finish(1, `no place of the desk said what step ${name} looks for`);
  }
  await shot(`${name}-hover`);
  await markShown();
  const dropped = Date.now();
  await drag('drop', ...found.at, files);
  const ended = await until(`(() => {
    const shown = [...document.querySelectorAll('.tilo-drop-progress:not([data-seen])')];
    const end = shown.find(box => ['done', 'failed'].includes(box.dataset.phase));
    return end ? JSON.stringify({phase: end.dataset.phase, said: end.innerText.trim()}) : '';
  })()`, `the end of step ${name}`, 120_000).then(JSON.parse);
  await sleep(2500);
  await shot(`${name}-end`);
  report.steps[name] = {hover: found.seen, ...ended, ms: Date.now() - dropped};
  console.error(`ok: ${name}: hovered ${JSON.stringify(found.seen)}, then ${ended.phase}: ${ended.said}`);
  return ended;
};

const key = async (key, code, modifiers = 0, text = '') => {
  await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key, code, modifiers, text});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key, code, modifiers});
};
// Open `app` from the desk's search, as a person does.
const openApp = async app => {
  await key('k', 'KeyK', 2);
  await sleep(800);
  for (const char of app) await tab.send('Input.insertText', {text: char});
  await sleep(1200);
  await key('Enter', 'Enter', 0, '\r');
  await sleep(3000);
};
// An empty place of the strip: a gap between windows, or the whole desk while none is open.
const strip = seen => ['strip', 'desk'].includes(seen.kind);
await dropStep('1-files-on-strip', ['plan.pdf', 'photo.png', 'notes.md', 'data.json'].map(file), strip);
await dropStep('2-folder-on-strip', [file('docs')], strip);
await dropStep('3-file-onto-files', [file('notes.md')], seen => /^Add to /.test(seen.note ?? ''));
await dropStep('4-picture-on-photos', [file('second.png')], seen => seen.note === 'Open in Photos');
await dropStep('5-picture-on-notes', [file('second.png')], seen => seen.note === 'Open in Notes');
await openApp('Calculator');
await dropStep('6-on-a-window-that-opens-no-files', [file('data.json')], seen => / doesn't open files$/.test(seen.note ?? ''));

// The large file: each step reports how long the page took from the drop to
// its end.
await dropStep('7-large-file-on-strip', [file('big.bin')], strip);

// 8. The desk's menu: "Upload a folder…", found in the desk's search, and the
// folder given to the chooser it opens.
await tab.send('Page.enable', {});
await tab.send('Page.setInterceptFileChooserDialog', {enabled: true});
let chooserOpened = null;
browser.on(message => {
  if (message.method === 'Page.fileChooserOpened') chooserOpened = message.params;
});
await markShown();
await key('k', 'KeyK', 2);
await sleep(800);
for (const char of 'Upload a folder') await tab.send('Input.insertText', {text: char});
await sleep(1200);
await key('Enter', 'Enter', 0, '\r');
const deadline = Date.now() + 30_000;
while (!chooserOpened && Date.now() < deadline) await sleep(250);
if (!chooserOpened) await finish(1, `the desk menu opening the folder chooser never happened: ${await text()}`);
const chose = await tab.send('DOM.setFileInputFiles', {files: [file('docs')], backendNodeId: chooserOpened.backendNodeId});
if (chose.error) await finish(1, `the chooser took no folder: ${chose.error.message}`);
report.steps['8-menu-upload'] = await until(`(() => {
  const end = [...document.querySelectorAll('.tilo-drop-progress:not([data-seen])')].find(box => ['done', 'failed'].includes(box.dataset.phase));
  return end ? JSON.stringify({phase: end.dataset.phase, said: end.innerText.trim()}) : '';
})()`, 'the end of the menu upload', 120_000).then(JSON.parse);
await sleep(1000);
await shot('8-menu-upload-end');
console.error(`ok: menu upload: ${JSON.stringify(report.steps['8-menu-upload'])}`);

await finish(0);
