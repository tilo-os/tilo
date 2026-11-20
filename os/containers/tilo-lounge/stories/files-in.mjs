// The files-in story: Ada brings files into a workspace three ways, then opens
// each of them there.
//
// She opens Tilo and makes a workspace of her own, drops a photo and a PDF on
// the page (a real drag and drop through the browser's own input), signs this
// computer in with `tilo login` from her page and copies a document into the
// workspace's folder, copies a spreadsheet in with the `tilo` command, and
// opens each file in the workspace.
//
// The computer she uses is a home of its own, and every `tilo` command the
// story runs there runs in one user and mount namespace, so the folder
// `tilo login` mounts with FUSE is the folder her `cp` writes into.
//
// usage: node files-in.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// Writes OUT_DIR/story.json (stories/recorder.mjs) and exits non-zero when a
// step failed or ran over its budget.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {deflateSync} from 'node:zlib';
import {sleep} from '../../../../tools/web/chrome.mjs';
import {SHELL, stepLimit} from '../integration/waits.mjs';
import {click, giveName, key, makeWorkspace, openWorkspace, searcher, settled, showsText as shows, waitUntil}
  from '../integration/shell.mjs';
import {computerAt} from './computer.mjs';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node files-in.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}
const fail = message => { throw new Error(message); };
const until = (tab, expression, what, timeout = stepLimit(3)) => waitUntil(tab, expression, what, timeout);
const {search} = searcher({until, failed: fail, dump: async () => {}});
// Ctrl+K, the search chord (Meta on a Mac).
const chord = async tab => (await tab.evaluate('/Mac/.test(navigator.platform)')) ? 4 : 2;
const arrived = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`;

// The files Ada has on her computer: a photo, a PDF, a document and a
// spreadsheet, written where a person's own files would be.
const where = join(out, 'files');
mkdirSync(where, {recursive: true});
const crcTable = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();
const crc32 = buffer => {
  let c = -1;
  for (const byte of buffer) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
};
/** A PNG, as a camera or a screenshot would leave one: a small picture. */
const photo = (width, height) => {
  const part = (type, data) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(type, 4, 'latin1');
    const tail = Buffer.alloc(4);
    tail.writeUInt32BE(crc32(Buffer.concat([Buffer.from(type, 'latin1'), data])), 0);
    return Buffer.concat([head, data, tail]);
  };
  const raw = Buffer.alloc((width * 3 + 1) * height);
  let at = 0;
  for (let y = 0; y < height; y++) {
    raw[at++] = 0;
    for (let x = 0; x < width; x++) {
      raw[at++] = Math.round(60 + 160 * (x / width));
      raw[at++] = Math.round(90 + 120 * (y / height));
      raw[at++] = Math.round(190 - 110 * ((x + y) / (width + height)));
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 2;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    part('IHDR', header),
    part('IDAT', deflateSync(raw)),
    part('IEND', Buffer.alloc(0)),
  ]);
};
/** A PDF with one page and a line of text on it, as a document from another
 * program arrives. */
const pdf = () => {
  const objects = [
    '',
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 360] /Contents 4 0 R '
      + '/Resources << /Font << /F1 5 0 R >> >> >>',
    null,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
  const words = 'BT /F1 24 Tf 60 300 Td (Spring trip) Tj ET';
  objects[4] = `<< /Length ${words.length} >>\nstream\n${words}\nendstream`;
  let text = '%PDF-1.4\n';
  const offsets = [0];
  for (let n = 1; n < objects.length; n++) {
    offsets[n] = Buffer.byteLength(text, 'latin1');
    text += `${n} 0 obj\n${objects[n]}\nendobj\n`;
  }
  const xref = Buffer.byteLength(text, 'latin1');
  text += `xref\n0 ${objects.length}\n0000000000 65535 f \n`;
  for (let n = 1; n < objects.length; n++) text += `${String(offsets[n]).padStart(10, '0')} 00000 n \n`;
  text += `trailer\n<< /Size ${objects.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(text, 'latin1');
};
const PHOTO = 'spring.png';
const PDF = 'ticket.pdf';
const DOCUMENT = 'letter.txt';
const SHEET = 'budget.csv';
writeFileSync(join(where, PHOTO), photo(320, 200));
writeFileSync(join(where, PDF), pdf());
writeFileSync(join(where, DOCUMENT), 'Dear Ada,\n\nThe keys are under the flowerpot.\n\nGrace\n');
writeFileSync(join(where, SHEET), 'item,cost\nrent,900\nfood,320\ncoffee,45\n');

// A drag of `paths` over the page at `(x, y)`, dropped there: what the
// browser sends for a person's own drag and drop.
const drop = async (tab, paths, {x, y}) => {
  const data = {items: [], files: paths, dragOperationsMask: 1};
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchDragEvent', {type: 'dragEnter', x, y, data});
  await tab.send('Input.dispatchDragEvent', {type: 'dragOver', x, y, data});
  await tab.send('Input.dispatchDragEvent', {type: 'drop', x, y, data});
};

// A press on a button of the page's own first screen (plain HTML over the
// desktop), where a person's pointer would land.
const pressPageButton = async (tab, id, what) => {
  const box = JSON.parse(await tab.evaluate(`(() => {
    const element = document.getElementById(${JSON.stringify(id)});
    if (!element || element.disabled) return '';
    const rect = element.getBoundingClientRect();
    if (!rect.width || !rect.height) return '';
    const x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
    const top = document.elementFromPoint(x, y);
    return JSON.stringify({x, y, covered: top && element.contains(top) ? null : top?.tagName ?? null});
  })()`).catch(() => '""'));
  if (!box) fail(`${what} is not on the page`);
  if (box.covered) fail(`${what} is covered by ${box.covered}: a person's press would land there`);
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
  }
};

const story = recorder({id: 'files-in', out, logs});
const WORKSPACE = 'Studio';
const computer = computerAt(out, {namespace: true});
let folder = null;
let error = null;
try {
  const ada = await story.person('Ada');
  const adaKeys = await chord(ada.tab);

  await story.step('Ada opens Tilo', async () => {
    const began = Date.now();
    await story.web.coldLoad(ada, site);
    await until(ada.tab, arrived, 'Ada’s page arriving', stepLimit(6));
    if (await ada.tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
      fail(`Ada’s page stopped: ${(await ada.tab.evaluate('(document.body.innerText || "").slice(0, 300)'))}`);
    }
    story.timing('Ada’s page load, to the desktop', Date.now() - began);
    story.web.scriptSize(ada);
  });

  await story.step('Ada gives their name', async () => {
    await click(ada.tab, SHELL.empty.x, SHELL.empty.y);
    await key(ada.tab, 'k', adaKeys);
    await until(ada.tab, 'globalThis.tiloMount?.state()?.searchOpen === true', 'the search opening');
    await search(ada.tab, adaKeys, 'set your name', 'Set your name');
    await giveName(ada.tab, 'Ada');
  });

  await story.step(`Ada makes a workspace, “${WORKSPACE}”`, async () => {
    await search(ada.tab, adaKeys, 'new workspace', 'New workspace');
    await makeWorkspace(ada.tab, WORKSPACE);
    await until(ada.tab, openWorkspace(WORKSPACE), `${WORKSPACE} opening`, stepLimit(10));
    await settled(ada.tab);
  });

  await story.step('Ada drops a photo and a PDF on the page', async () => {
    // Files is open on the workspace's folder, where she watches them arrive.
    const files = `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === 'so.tilo.file-explorer')`;
    await search(ada.tab, adaKeys, 'files', 'Files', 'so.tilo.file-explorer');
    await until(ada.tab, files, 'Files on Ada’s screen', stepLimit(4));
    await drop(ada.tab, [join(where, PHOTO), join(where, PDF)], SHELL.empty);
    // The shell says how the copy went where it fell, then opens each file.
    const progress = `(() => {
      const shown = document.querySelector('.tilo-drop-progress');
      return shown && ['done', 'failed'].includes(shown.dataset.phase)
        ? JSON.stringify({phase: shown.dataset.phase, text: shown.textContent}) : '';
    })()`;
    const copied = JSON.parse(await until(ada.tab, progress, 'the drop’s copy finishing', stepLimit(4)));
    if (copied.phase === 'failed') fail(`the drop did not copy the files in: ${copied.text}`);
    // Then each one opened in the app that reads it.
    const opened = app => `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(app)})`;
    await until(ada.tab, opened('so.tilo.photos'), 'the photo opening in Photos', stepLimit(4));
    await until(ada.tab, opened('so.tilo.pdf-viewer'), 'the PDF opening in PDF Viewer', stepLimit(4));
    await shows(ada.tab, PHOTO, `${PHOTO} in Files`, stepLimit(4));
    await shows(ada.tab, PDF, `${PDF} in Files`, stepLimit(4));
  });

  await story.step('Ada runs `tilo login` and copies a document into the workspace folder', async () => {
    // Her page hands the computer a login code, as `tilo login` asks for.
    await search(ada.tab, adaKeys, 'connect an agent or computer', 'Connect an agent or computer');
    const prompt = await until(ada.tab, `(() => {
      const offer = document.getElementById('first-screen-offer')?.textContent ?? '';
      return /login [2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4}/.test(offer) ? offer : '';
    })()`, 'the setup prompt with a login code', stepLimit(6));
    const code = prompt.match(/login ([2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})/)[1];
    const loggedIn = await computer.run(['login', code, '--site', site], {timeout: stepLimit(12)});
    if (loggedIn.code !== 0) fail(`tilo login failed (${loggedIn.code}): ${loggedIn.stderr}${loggedIn.stdout}`);
    if (!/Logged in/.test(loggedIn.stdout)) fail(`tilo login did not say it logged in: ${loggedIn.stdout.slice(-600)}`);
    // The tab the code came from follows it and says the computer is in. With
    // no agent tool there, the screen stays on the step after that, and the
    // person closes it to get back to their workspace.
    await until(ada.tab,
      `(document.getElementById('first-screen-steps')?.textContent ?? '').includes('logged in as you')`,
      'the screen saying this computer logged in', stepLimit(6));
    await pressPageButton(ada.tab, 'first-screen-skip', 'the screen’s Skip for now');
    await until(ada.tab, `document.documentElement.dataset.firstScreen === undefined`, 'the setup screen leaving', stepLimit(6));
    await until(ada.tab, `globalThis.tiloMount?.state?.()?.phase === 'presented'`, 'Ada’s workspace on screen again', stepLimit(6));
    // The workspace's folder, mounted on her computer: what `tilo folders` says.
    // The holder mounts it a moment after the login returns, so it is asked
    // until it answers with the folder.
    let shown = null;
    const deadline = Date.now() + stepLimit(4);
    let listing = '';
    while (!shown) {
      const listed = await computer.run(['folders', '--json'], {timeout: stepLimit(3)});
      listing = listed.stdout;
      if (listed.code === 0) {
        const said = JSON.parse(listed.stdout.trim().split('\n').at(-1));
        shown = (said.peer?.mounted ?? []).find(entry => entry.name === WORKSPACE) ?? null;
      }
      if (!shown && Date.now() > deadline) fail(`no folder of ${WORKSPACE} is mounted here: ${listing.slice(0, 800)}`);
      if (!shown) await sleep(1000);
    }
    folder = {path: shown.at, shown: shown.shown};
    // Her document, copied into the folder as a person copies one.
    const copied = await computer.line(`cp ${JSON.stringify(join(where, DOCUMENT))} ${JSON.stringify(folder.path)}/`);
    if (copied.code !== 0) fail(`copying ${DOCUMENT} into ${folder.path} failed: ${copied.stderr}`);
    // It is in the workspace: the Files window she has open shows it.
    await shows(ada.tab, DOCUMENT, `the copied ${DOCUMENT} in Files`, stepLimit(8));
  });

  await story.step('Ada copies a spreadsheet in with the `tilo` command', async () => {
    const put = await computer.run(['files', 'put', join(where, SHEET), '-w', WORKSPACE], {timeout: stepLimit(8)});
    if (put.code !== 0) fail(`tilo files put failed (${put.code}): ${put.stderr}${put.stdout}`);
    await shows(ada.tab, SHEET, `the spread sheet ${SHEET} in Files`, stepLimit(8));
  });

  await story.step('Ada opens each file in the workspace', async () => {
    // Each one searched for by its name, as a person finds a file, and opened
    // from the palette in the app that reads it: a picture in Photos, a PDF in
    // PDF Viewer, a text document in Notes and a table in Sheets. Where the
    // file's own words are on screen, they are read back too.
    const files = [
      {name: PHOTO, app: 'so.tilo.photos'},
      {name: PDF, app: 'so.tilo.pdf-viewer'},
      {name: DOCUMENT, app: 'so.tilo.notes', words: 'keys are under the flowerpot'},
      {name: SHEET, app: 'so.tilo.sheets', words: 'rent'},
    ];
    for (const file of files) {
      await search(ada.tab, adaKeys, file.name, file.name, file.app);
      const opened = `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(file.app)})`;
      await until(ada.tab, opened, `${file.name} opening in ${file.app}`, stepLimit(4));
      if (file.words) await shows(ada.tab, file.words, `${file.name} shown in its window`, stepLimit(4));
      await sleep(500);
    }
  });
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
} finally {
  computer.close();
}
// A last look at the screen, so the recording ends on what Ada saw.
await sleep(1500);
const result = await story.finish({error, extra: {computer: {folder}}});
console.log(JSON.stringify({completed: result.completed, withinBudget: result.withinBudget, brokeAt: result.brokeAt}, null, 2));
process.exit(result.completed && result.withinBudget ? 0 : 1);
