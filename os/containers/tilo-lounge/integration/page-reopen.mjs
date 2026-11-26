// A page's app request held past the time the server waits for it: the
// server lets the record go and opens it again from what the workspace
// holds, and the page shows it again where the person left it and asks again
// what they did meanwhile. The app workers are stopped themselves for longer
// than the server waits, so the request really outlasts it, and what the
// person typed while they were stopped is what the page and the workspace
// hold afterwards.
//
// usage: node page-reopen.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR
//
// Fails when the page reports an exception, when it gives up on the page
// instead of showing it again, or when a note typed in during the stall is
// not there afterwards.
import {execFileSync} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, pinnedCertificates, servedCertificate, sleep} from '../../../../tools/web/chrome.mjs';
import {confirmRecoveryWords} from './waits.mjs';

const [site, server, workspace, out] = process.argv.slice(2);
if (!site || !server || !workspace || !out) {
  console.error('usage: node page-reopen.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

// How long the workers are held: past the minute the server waits for one
// request, so the one in flight is let go and the document opened again.
const HELD_MS = Number(process.env.TILO_REOPEN_HELD_SECONDS ?? 66) * 1000;
// The first title typed while the app answers, and what is added to it while
// the workers are held.
const TITLE = 'Held open';
const TYPED_WHILE_HELD = ' past the wait';

const report = {site, server, workspace, steps: [], exceptions: []};
const problems = [];
const browser = await launch({args: [pinnedCertificates([await servedCertificate(site), await servedCertificate(server)])]});
let tab = null;
const step = (name, data) => { report.steps.push({name, ...data}); console.log(name, JSON.stringify(data)); };
const shot = async name => { const png = await tab.screenshot().catch(() => null); if (png) writeFileSync(join(out, `${name}.png`), png); };
const until = async (expression, timeout = 15_000) => {
  const began = Date.now();
  const ok = await tab.waitFor(`(() => !!(${expression}))()`, {timeout}).then(() => true).catch(() => false);
  return ok ? Date.now() - began : null;
};
const type = async text => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    await sleep(8);
  }
};
/** The app worker processes the server started: what answers a page's asks. */
const workers = () => {
  try {
    return execFileSync('pgrep', ['-f', 'tilo_agent_app_worker'], {encoding: 'utf8'})
      .split('\n').map(line => line.trim()).filter(Boolean).map(Number);
  } catch {
    return [];
  }
};
const signal = (pids, name) => {
  for (const pid of pids) {
    try { process.kill(pid, name); } catch { /* it ended on its own */ }
  }
};

const title = `document.querySelector('#tilo-page textarea')`;
const state = `(() => { const page = document.getElementById('tilo-page'); return JSON.stringify({state: page?.dataset.state ?? null, refused: page?.dataset.refused ?? null, working: page?.hasAttribute('data-working') ?? false}); })()`;

try {
  tab = await browser.tab();
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  tab.log.length = 0;

  // The site's entry makes this browser a device of the workspace: it either
  // hands it to the page there, or opens its desktop, which this page's walk
  // does not need.
  await tab.navigate(site);
  let arrived = null;
  // A cold Lounge's first admission takes a while on a busy node: the walk
  // waits four minutes for it, as page-notes does, and presses the card's way
  // in once.
  for (let i = 0; i < 960; i++) {
    // The person is founded on the way in: their 24 recovery words are shown
    // once and hold the page until kept, so the card is confirmed as it comes.
    await confirmRecoveryWords(tab);
    const seen = await tab.evaluate(`(() => {
      if (location.pathname.startsWith('/w/')) return 'handed';
      if (document.documentElement.dataset.arrival === 'stopped') return 'stopped';
      if (document.documentElement.dataset.arrival === 'done') return 'done';
      const join = [...document.querySelectorAll('button')].find(b => /^Join\b/i.test(b.textContent.trim()));
      if (join && !join.disabled && !join.dataset.pressed) { join.dataset.pressed = '1'; join.click(); }
      return '';
    })()`).catch(() => '');
    if (seen === 'handed' || seen === 'done') { arrived = {state: seen, ms: i * 250}; break; }
    if (seen === 'stopped') { arrived = {state: seen, ms: i * 250}; break; }
    await sleep(250);
  }
  step('arrival', arrived ?? {state: null});
  if (arrived === null || arrived.state === 'stopped') problems.push('the browser did not get into the workspace');

  await tab.navigate(`${site}/w/${workspace}/so.tilo.notes`);
  if (await until('document.querySelector("#tilo-page [data-p]")', 60_000) === null) problems.push('the page never showed');
  await tab.evaluate(`[...document.querySelectorAll('#tilo-page [data-a=activate]')].find(e => e.getAttribute('aria-label') === 'New note' || e.textContent.trim() === 'New note').click()`);
  if (await until(`document.activeElement === ${title}`, 15_000) === null) problems.push('a new note did not put the caret in its title');
  await type(TITLE);
  if (await until(`${title}?.value === ${JSON.stringify(TITLE)}`, 15_000) === null) problems.push('the title typed while the app answered did not show');
  await sleep(500);
  await shot('01-before-the-stall');

  // Hold the app: what the person does now is asked of a worker that cannot
  // answer, and the server lets the request go past its wait.
  const held = workers();
  step('workers held', {pids: held});
  if (!held.length) problems.push('no app worker was running to hold');
  signal(held, 'SIGSTOP');
  // A worker the server starts again while the record is let go is held too.
  await sleep(2000);
  signal(workers().filter(pid => !held.includes(pid)), 'SIGSTOP');

  const heldAt = Date.now();
  await type(TYPED_WHILE_HELD);
  step('typed while held', {value: await tab.evaluate(`${title}?.value ?? null`), at: Date.now() - heldAt});
  // Past the wait, so the request in flight is let go and the document is
  // opened again from what the workspace holds.
  await sleep(Math.max(0, HELD_MS - (Date.now() - heldAt)));
  signal(workers(), 'SIGCONT');
  step('released', {after: Date.now() - heldAt});

  const whole = JSON.stringify(`${TITLE}${TYPED_WHILE_HELD}`);
  const back = await until(`${title}?.value === ${whole}`, 120_000);
  const after = JSON.parse(await tab.evaluate(state));
  step('after the reopen', {ms: back, ...after, value: await tab.evaluate(`${title}?.value ?? null`)});
  await shot('02-after-the-reopen');
  if (back === null) problems.push(`the page did not show the title again (it held ${JSON.stringify(await tab.evaluate(`${title}?.value ?? null`))})`);
  if (after.refused) problems.push(`the page was refused after the reopen (${after.refused})`);

  // The page works again: a note it opens now is asked of the app as any
  // other, and what the workspace holds is what its list says.
  await tab.evaluate(`[...document.querySelectorAll('#tilo-page [data-a=activate]')].find(e => e.getAttribute('aria-label') === 'New note' || e.textContent.trim() === 'New note').click()`);
  if (await until(`document.activeElement === ${title}`, 30_000) === null) problems.push('a new note after the reopen did not put the caret in its title');
  await type('After the wait');
  if (await until(`${title}?.value === 'After the wait'`, 30_000) === null) problems.push('typing after the reopen did not show in the field');
  await sleep(800);
  const rows = await tab.evaluate(`[...document.querySelectorAll('#tilo-page [data-a=activate][data-t]')].filter(r => (r.innerText || '').includes('Held open')).length`);
  step('the list keeps the note', {rows, state: JSON.parse(await tab.evaluate(state))});
  if (!rows) problems.push('the note typed while the app was held is not in the list');
  await shot('03-list-after-the-reopen');
} catch (error) {
  report.error = String(error?.stack ?? error).slice(0, 4000);
  problems.push(report.error);
} finally {
  report.exceptions = (tab?.log ?? []).filter(line => /^EXC|error/i.test(line)).slice(-20);
  report.problems = problems;
  writeFileSync(join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
  console.log('report', JSON.stringify({problems, exceptions: report.exceptions}));
  await Promise.race([browser.close().catch(() => {}), sleep(8000)]).catch(() => {});
  process.exit(problems.length ? 1 : 0);
}
