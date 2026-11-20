// A person on a workspace's Notes as a web page: the page the server renders,
// in headless Chromium beside the Lounge, walked as a person walks it — the
// site's entry makes the browser a device, the page opens, a note is opened and
// typed in, the browser goes Back and Forward, the page is reloaded at the
// note's address, looked at at a phone's width, and opened again from a second
// browser profile. What each step saw is kept, with a picture of it.
//
// usage: node page-notes.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR
//
// Fails only when the page itself reports an exception, or when a step a person
// would notice never happened; what looks right is the reviewer's to judge.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, pinnedCertificates, servedCertificate, sleep} from '../../../../tools/web/chrome.mjs';

const [site, server, workspace, out] = process.argv.slice(2);
if (!site || !server || !workspace || !out) {
  console.error('usage: node page-notes.mjs SITE_URL SERVER_ORIGIN WORKSPACE OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

const report = {site, server, workspace, steps: [], exceptions: []};
const siteCertificate = await servedCertificate(site);
const serverCertificate = await servedCertificate(server);
const browser = await launch({args: [pinnedCertificates([siteCertificate, serverCertificate])]});
let tab = null;
const problems = [];

const step = (name, data) => { report.steps.push({name, ...data}); console.log(name, JSON.stringify(data)); };
const shot = async name => { const png = await tab.screenshot().catch(() => null); if (png) writeFileSync(join(out, `${name}.png`), png); };
const until = async (expression, timeout = 10_000) => {
  const began = Date.now();
  const ok = await tab.waitFor(`(() => !!(${expression}))()`, {timeout}).then(() => true).catch(() => false);
  return ok ? Date.now() - began : null;
};

try {
  // What the page's own load put on the wire: the document with its script
  // inlined, and nothing else unless something else was fetched.
  const wire = {doc: 0, script: 0, requests: [], track: new Map()};
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab?.sessionId) return;
    if (method === 'Network.responseReceived') {
      wire.requests.push(`${params.type} ${new URL(params.response.url).pathname}`);
      if (params.type === 'Document') wire.track.set(params.requestId, 'doc');
      else if (params.type === 'Script') wire.track.set(params.requestId, 'script');
    }
    if (method === 'Network.loadingFinished' && wire.track.has(params.requestId)) {
      wire[wire.track.get(params.requestId)] += params.encodedDataLength;
    }
  });
  const resetWire = () => { wire.doc = 0; wire.script = 0; wire.requests = []; wire.track = new Map(); };

  const arrive = async ({context, label}) => {
    tab = await browser.tab({context});
    await tab.send('Network.enable', {}).catch(() => {});
    tab.log.length = 0;
    await tab.navigate(site);
    // The entry offers the way in on its card: pressed as a person presses it.
    for (let i = 0; i < 240; i++) {
      const state = await tab.evaluate(`(() => {
        if (['done', 'stopped'].includes(document.documentElement.dataset.arrival)) return document.documentElement.dataset.arrival;
        const join = [...document.querySelectorAll('button')].find(b => /^(Join|Join Tilo|Continue|Accept|Open)$/i.test(b.textContent.trim()));
        if (join && !join.disabled && !join.dataset.pressed) { join.dataset.pressed = '1'; join.click(); }
        return '';
      })()`).catch(() => '');
      if (state === 'done' || state === 'stopped') { step(`${label}: arrival`, {state, ms: i * 250}); break; }
      await sleep(250);
    }
    step(`${label}: devices this browser kept`, {
      kept: await tab.evaluate(`(() => { try { return Object.keys(JSON.parse(localStorage.getItem('tilo-browser-devices') ?? '{}')?.exact ?? {}).length; } catch { return 0; } })()`),
    });
    await shot(`${label}-arrived`);
  };

  // The page, as a person reads it: its address, its tab, what it shows.
  const look = async name => {
    let seen;
    try {
      seen = JSON.parse(await tab.evaluate(`(() => {
        const page = document.getElementById('tilo-page');
        return JSON.stringify({
          path: location.pathname + location.hash,
          title: document.title,
          history: history.length,
          state: page?.dataset.state ?? null,
          refused: page?.dataset.refused ?? null,
          elements: page?.querySelectorAll('[data-p]').length ?? 0,
          selected: [...(page?.querySelectorAll('[aria-selected=true]') ?? [])].map(e => e.dataset.t ?? e.getAttribute('aria-label') ?? '').slice(0, 3),
          titleField: page?.querySelector('textarea')?.value ?? null,
          fields: [...(page?.querySelectorAll('textarea, input[type=text], [contenteditable]') ?? [])].map(e => (e.value ?? e.innerText ?? '').slice(0, 40)),
          headings: [...(page?.querySelectorAll('h1, h2') ?? [])].map(e => e.innerText.trim().slice(0, 40)),
        });
      })()`));
    } catch (error) {
      seen = {error: String(error)};
    }
    step(name, seen);
    await shot(name);
    return seen;
  };

  const openPage = async label => {
    resetWire();
    await tab.navigate(`${site}/w/${workspace}/notes`);
    const shown = await until('document.querySelector("#tilo-page [data-p]")', 60_000);
    step(`${label}: page shown`, {
      ms: shown,
      state: await tab.evaluate(`document.getElementById('tilo-page')?.dataset.state ?? 'shown'`),
      marked: Math.round(await tab.evaluate(`performance.getEntriesByName('tilo-page-shown')[0]?.startTime ?? 0`).catch(() => 0)),
      documentBytes: wire.doc,
      scriptBytes: wire.script,
      requests: wire.requests.slice(0, 8),
    });
    if (shown === null) problems.push(`${label}: the page never showed`);
  };

  const type = async text => {
    for (const ch of text) {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
      await sleep(8);
    }
  };
  // A press as a person gives one: the pointer goes where the thing is drawn,
  // and what it lands on is reported, which is how a covered control shows.
  const click = async expression => {
    const at = await tab.evaluate(`(() => {
      const el = ${expression};
      if (!el) return '';
      el.scrollIntoView({block: 'nearest'});
      const r = el.getBoundingClientRect();
      const x = r.x + Math.min(r.width / 2, 40);
      const y = r.y + r.height / 2;
      const on = document.elementFromPoint(x, y);
      return JSON.stringify({x, y, on: on === el || el.contains(on) ? 'the thing' : (on?.outerHTML ?? '').slice(0, 120)});
    })()`);
    if (!at) return '';
    const {x, y, on} = JSON.parse(at);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
    }
    return on;
  };

  const rows = `[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]`;
  const title = `document.querySelector('#tilo-page textarea')`;

  await arrive({label: 'first'});
  await openPage('first open');
  await look('01-notes-wide');

  await click(`document.querySelector('#tilo-page [data-a=activate][aria-label="New note"]')`);
  if (await until(`document.activeElement === ${title}`, 15_000) === null) problems.push('a new note did not put the caret in its title');
  await type('Garden plan for the spring');
  if (await until(`${title}?.value === 'Garden plan for the spring'`, 15_000) === null) problems.push('typing a title did not show in the field');
  await sleep(800);
  await look('02-typed');

  await click(`document.querySelector('#tilo-page [data-a=activate][aria-label="New note"]')`);
  await until(`document.activeElement === ${title}`, 15_000);
  await type('Groceries for the week');
  await until(`${title}?.value === 'Groceries for the week'`, 15_000);
  await sleep(800);
  await look('03-second-note');

  await click(`${rows}.find(r => (r.innerText || '').includes('Garden')) ?? null`);
  if (await until(`${title}?.value === 'Garden plan for the spring'`, 15_000) === null) problems.push('a row click did not open that note');
  await sleep(500);
  await look('04-other-note');

  await tab.evaluate('history.back()');
  if (await until(`${title}?.value === 'Groceries for the week'`, 15_000) === null) problems.push('Back did not return to the note before');
  await sleep(500);
  await look('05-after-back');

  await tab.evaluate('history.forward()');
  if (await until(`${title}?.value === 'Garden plan for the spring'`, 15_000) === null) problems.push('Forward did not return to the note after');
  await sleep(500);
  await look('06-after-forward');

  resetWire();
  await tab.reload();
  const reloaded = await until('document.querySelector("#tilo-page [data-p]")', 60_000);
  await sleep(800);
  step('reload', {
    ms: reloaded,
    documentBytes: wire.doc,
    scriptBytes: wire.script,
    marked: Math.round(await tab.evaluate(`performance.getEntriesByName('tilo-page-shown')[0]?.startTime ?? 0`).catch(() => 0)),
  });
  const afterReload = await look('07-reloaded');
  if (afterReload.titleField !== 'Garden plan for the spring') problems.push(`a reload did not show the note its address names (it showed ${JSON.stringify(afterReload.titleField)})`);

  await tab.send('Emulation.setDeviceMetricsOverride', {width: 390, height: 844, deviceScaleFactor: 2, mobile: true});
  await sleep(800);
  await look('08-phone');
  const pressed = await click(`[...document.querySelectorAll('#tilo-page [data-a=back]')].find(e => e.offsetParent) ?? null`);
  await sleep(900);
  step('phone back', {pressed, visibleRows: await tab.evaluate(`${rows}.filter(r => r.offsetParent).length`)});
  await look('09-phone-back');
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});

  const context = await browser.context();
  await arrive({context, label: 'second'});
  await openPage('second profile');
  await look('10-second-profile');
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
