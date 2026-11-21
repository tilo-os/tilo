// The page boots for real: the built bundle in headless Chromium beside a
// running server, as a person's browser loads it. The tab arrives at the site,
// joins the Lounge's workspace, is handed to the page, and Notes shows. They
// make a note, type its title, and reload: the title is still there, because
// it is the workspace's and not the page's. (What a page can do in each app is
// walked by page-notes.mjs.)
//
// What it catches is what compiles and passes every unit test yet stops a real
// page: the page's script calling something the bundle lacks, a read of the
// browser's storage before the page was let in, or a page that shows and keeps
// none of what is done on it. Any exception the page throws, and any console
// line that reports a failure, fails the run, even when the page went on.
//
// usage: node page-boot.mjs SITE_URL SERVER_ORIGIN OUT_DIR
//
// Prints a JSON report and keeps the tab's console and a screenshot in
// OUT_DIR; exits non-zero when a step did not happen or the page said it
// failed.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {stepLimit, shellMetrics, machine} from './waits.mjs';

const [site, server, out] = process.argv.slice(2);
if (!site || !server || !out) {
  console.error('usage: node page-boot.mjs SITE_URL SERVER_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const mark = clock();
const report = {site, server, at: new Date().toISOString()};
const browser = await launch();
// A person who has been past the first screen: their page opens the Lounge.
const tab = await browser.tab();

// What the page said that is not a script's exception or a console error:
// the browser's own log and requests that failed, kept for the report only.
const loading = [];
const asked = new Map();
browser.on(({sessionId, method, params}) => {
  if (sessionId !== tab.sessionId) return;
  if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
  else if (method === 'Network.loadingFailed') {
    loading.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
  } else if (method === 'Log.entryAdded') {
    loading.push(`LOG ${params.entry.level} ${params.entry.text}`.slice(0, 600));
  }
});
await tab.send('Network.enable', {}).catch(() => {});
await tab.send('Log.enable', {}).catch(() => {});
await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics)
  .catch(() => {});

// Every exception the page threw and every console line that reports a
// failure: an error, or any line that says the runtime panicked.
const failures = () => tab.log.filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));

const finish = async (code, message) => {
  if (message) report.error = message;
  report.failures = failures();
  report.console = tab.log.slice(-60);
  report.loading = loading.slice(-30);
  report.text = await tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 2000)`)
    .catch(() => '');
  writeFileSync(join(out, 'console.log'), [...tab.log, '--- loading', ...loading].join('\n'));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'page.png'), shot);
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  await browser.close().catch(() => {});
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
    console.error(`the machine as it failed: ${machine().text}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const fail = message => finish(1, message);
const checkPage = async step => {
  const found = failures();
  if (found.length) await fail(`the page failed while ${step}: ${found.slice(0, 5).join(' | ')}`);
};

// Wait until `expression` holds in the page, or fail naming what the page
// showed instead. A failure the page reports ends the wait at once.
const until = async (expression, what, timeout = stepLimit(4)) => {
  const answered = await tab.waitFor(expression, {timeout}).then(value => ({value}), () => ({}));
  await checkPage(what);
  if (answered.value) return answered.value;
  const shown = await tab.evaluate(`JSON.stringify({
    arrival: document.documentElement.dataset.arrival ?? null,
    path: location.pathname,
    refused: document.getElementById('tilo-page')?.dataset.refused ?? null,
    state: document.getElementById('tilo-page')?.dataset.state ?? null,
    asks: document.getElementById('tilo-page')?.dataset.asks ?? null,
    answers: document.getElementById('tilo-page')?.dataset.answers ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
  })`).catch(() => 'unreadable');
  await fail(`${what} never happened: ${shown}`);
};

// Keys as a person types them.
const type = async text => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    await sleep(8);
  }
};
const press = async what => {
  await tab.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find(b => ${what});
    if (button && !button.dataset.pressed) { button.dataset.pressed = '1'; button.click(); }
  })()`);
};

// 1. The page boots and arrives.
await tab.navigate(site);
// The site's entry admits this browser, then hands it to the page.
await press(`/^Join\\b/i.test(b.textContent.trim())`);
await until(`document.documentElement.dataset.arrival === 'done'`, 'the page arriving');
console.log('ok: the page booted and arrived');
mark('the page arrived');
report.arrivedMs = Date.now() - Date.parse(report.at);

// 2. The entry handed it to the workspace, and the page shows what is there.
const path = await tab.evaluate('location.pathname');
if (!path.startsWith('/w/')) await fail(`the entry did not hand the browser to a page (it is at ${path})`);
report.workspace = path.split('/')[2];
await until(`document.querySelector('#tilo-page [data-p]')`, 'the page showing the workspace');
console.log(`ok: the page shows the workspace ${report.workspace}`);
mark('the page showed the workspace');

// 3. Notes: a note is made, its title typed, and it is the workspace's.
await tab.navigate(`${site}/w/${report.workspace}/notes`);
await until(`document.querySelector('#tilo-page [data-a=activate][aria-label="New note"]')`, 'Notes showing');
await press(`b.getAttribute('aria-label') === 'New note'`);
const title = `document.querySelector('#tilo-page textarea')`;
await until(`document.activeElement === ${title}`, 'the caret in the new note\'s title');
await type('Booted from the page');
await until(`${title}?.value === 'Booted from the page'`, 'the typed title showing');
// The note's row says it once the workspace holds it: what proves it was kept
// is the row, not the field the person typed in.
await until(`[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]
  .some(row => (row.innerText || '').includes('Booted from the page'))`, 'the workspace holding the title');
console.log('ok: a note typed on the page is the workspace\'s');
mark('a note was kept');

await tab.reload();
await until(`document.querySelector('#tilo-page [data-p]')`, 'the page showing after a reload');
await until(`[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]
  .some(row => (row.innerText || '').includes('Booted from the page'))`, 'the note after a reload');
console.log('ok: the note is there after a reload');
mark('the note was there after a reload');

report.ok = true;
await finish(0);
