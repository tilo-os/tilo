// A person on a workspace as a web page: the page the server renders, in
// headless Chromium beside the Lounge, walked as a person walks it — the
// site's entry makes the browser a device, the page opens, a note is opened
// and typed in, the browser goes Back and Forward, the page is reloaded at the
// note's address, looked at at a phone's width, opened again from a second
// browser profile, and then the workspace's own page and each app a person
// opens in their first hour. What each step saw is kept, with a picture of it.
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

// A person's window, in the two shapes the page lays itself out for: a
// desktop's, where the columns stand beside each other, and a phone's.
const WIDE = {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false};
const PHONE = {width: 390, height: 844, deviceScaleFactor: 2, mobile: true};
const sized = async size => tab.send('Emulation.setDeviceMetricsOverride', size);

const step = (name, data) => { report.steps.push({name, ...data}); console.log(name, JSON.stringify(data)); };
const shot = async name => { const png = await tab.screenshot().catch(() => null); if (png) writeFileSync(join(out, `${name}.png`), png); };
const until = async (expression, timeout = 10_000) => {
  const began = Date.now();
  const ok = await tab.waitFor(`(() => !!(${expression}))()`, {timeout}).then(() => true).catch(() => false);
  return ok ? Date.now() - began : null;
};

try {
  // What the page's own load put on the wire: the document with its script
  // inlined, and nothing else unless something else was fetched. What the
  // site's entry costs before it is read is counted the same way, so the web
  // build's own size has a number.
  const wire = {doc: 0, script: 0, other: 0, requests: [], track: new Map()};
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab?.sessionId) return;
    if (method === 'Network.responseReceived') {
      wire.requests.push(`${params.type} ${new URL(params.response.url).pathname}`);
      wire.track.set(params.requestId,
        params.type === 'Document' ? 'doc' : params.type === 'Script' ? 'script' : 'other');
    }
    if (method === 'Network.loadingFinished' && wire.track.has(params.requestId)) {
      wire[wire.track.get(params.requestId)] += params.encodedDataLength;
    }
  });
  const resetWire = () => { wire.doc = 0; wire.script = 0; wire.other = 0; wire.requests = []; wire.track = new Map(); };

  // The entry offers the way in on its card, pressed as a person presses it.
  // Once the browser is a device of the workspace the entry hands it to the
  // page, so a person lands on the workspace itself.
  const arrive = async ({context, label}) => {
    tab = await browser.tab({context});
    await tab.send('Network.enable', {}).catch(() => {});
    // The browser's own window is small, and a page narrower than its widest
    // column shows one column at a time, as a narrow window does: a person at
    // a desktop sees every column beside the others.
    await sized(WIDE);
    tab.log.length = 0;
    // What the entry itself costs to read, before it hands the browser over.
    resetWire();
    await tab.navigate(site);
    let handed = null;
    let arrival = null;
    for (let i = 0; i < 960; i++) {
      const state = await tab.evaluate(`(() => {
        if (location.pathname.startsWith('/w/')) return 'handed';
        const arrival = document.documentElement.dataset.arrival;
        if (arrival === 'stopped') return 'stopped';
        if (arrival === 'done') return 'done';
        const join = [...document.querySelectorAll('button')].find(b => /^(Join|Join Tilo|Continue|Accept|Open)$/i.test(b.textContent.trim()));
        if (join && !join.disabled && !join.dataset.pressed) { join.dataset.pressed = '1'; join.click(); }
        return '';
      })()`).catch(() => '');
      if (state === 'handed') { handed = i * 250; break; }
      if (state === 'stopped' || state === 'done') { arrival = state; break; }
      await sleep(250);
    }
    step(`${label}: arrival`, {
      handed,
      arrival,
      path: await tab.evaluate('location.pathname').catch(() => null),
      entry: {documentBytes: wire.doc, scriptBytes: wire.script, otherBytes: wire.other, requests: wire.requests.length},
    });
    if (handed === null) problems.push(`${label}: the entry did not hand the browser to the page`);
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
          asks: page?.dataset.asks ?? null,
          answers: page?.dataset.answers ?? null,
          asksLost: page?.dataset.asksLost ?? null,
          elements: page?.querySelectorAll('[data-p]').length ?? 0,
          selected: [...(page?.querySelectorAll('[aria-selected=true]') ?? [])].map(e => e.dataset.t ?? e.getAttribute('aria-label') ?? '').slice(0, 3),
          titleField: page?.querySelector('textarea')?.value ?? null,
          fields: [...(page?.querySelectorAll('textarea, input[type=text], [contenteditable]') ?? [])].map(e => (e.value ?? e.innerText ?? '').slice(0, 40)),
          headings: [...(page?.querySelectorAll('h1, h2') ?? [])].map(e => e.innerText.trim().slice(0, 40)),
          // What the page holds, for a reviewer of a step that did not do what
          // a person would expect: the elements it shows, in order.
          html: (page?.innerHTML ?? '').slice(0, 16000),
        });
      })()`));
    } catch (error) {
      seen = {error: String(error)};
    }
    step(name, seen);
    await shot(name);
    return seen;
  };

  // A page of the workspace, by the app's name: no name is the workspace's
  // own front page.
  const openPage = async (label, app = 'notes') => {
    resetWire();
    await tab.navigate(`${site}/w/${workspace}${app ? `/${app}` : ''}`);
    const shown = await until('document.querySelector("#tilo-page [data-p]")', 60_000);
    step(`${label}: page shown`, {
      ms: shown,
      state: await tab.evaluate(`document.getElementById('tilo-page')?.dataset.state ?? 'shown'`),
      marked: Math.round(await tab.evaluate(`performance.getEntriesByName('tilo-page-shown')[0]?.startTime ?? 0`).catch(() => 0)),
      documentBytes: wire.doc,
      scriptBytes: wire.script,
      otherBytes: wire.other,
      requests: wire.requests.slice(0, 8),
    });
    if (shown === null) problems.push(`${label}: the page never showed`);
    return shown;
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
    if (!at) return 'nothing to press';
    const {x, y, on} = JSON.parse(at);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
    }
    return on;
  };
  // A press the walk needs to have landed: a person who finds nothing there,
  // or finds something covering it, has a page that does not work.
  const press = async (expression, what) => {
    const on = await click(expression);
    if (on === 'nothing to press') problems.push(`there was no ${what} to press`);
    else if (on !== 'the thing') problems.push(`pressing ${what} landed on ${on}`);
    return on;
  };

  const rows = `[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]`;
  const title = `document.querySelector('#tilo-page textarea')`;

  await arrive({label: 'first'});
  await openPage('first open');
  await look('01-notes-wide');

  await press(`document.querySelector('#tilo-page [data-a=activate][aria-label="New note"]')`, 'New note');
  if (await until(`document.activeElement === ${title}`, 15_000) === null) problems.push('a new note did not put the caret in its title');
  await type('Garden plan for the spring');
  if (await until(`${title}?.value === 'Garden plan for the spring'`, 15_000) === null) problems.push('typing a title did not show in the field');
  await sleep(800);
  await look('02-typed');

  await press(`document.querySelector('#tilo-page [data-a=activate][aria-label="New note"]')`, 'New note');
  if (await until(`document.activeElement === ${title}`, 15_000) === null) problems.push('a second new note did not put the caret in its title');
  await type('Groceries for the week');
  if (await until(`${title}?.value === 'Groceries for the week'`, 15_000) === null) problems.push('typing the second title did not show in the field');
  await sleep(800);
  await look('03-second-note');

  await press(`${rows}.find(r => (r.innerText || '').includes('Garden')) ?? null`, 'the Garden note in the list');
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
    otherBytes: wire.other,
    marked: Math.round(await tab.evaluate(`performance.getEntriesByName('tilo-page-shown')[0]?.startTime ?? 0`).catch(() => 0)),
  });
  const afterReload = await look('07-reloaded');
  if (afterReload.titleField !== 'Garden plan for the spring') problems.push(`a reload did not show the note its address names (it showed ${JSON.stringify(afterReload.titleField)})`);

  await sized(PHONE);
  await sleep(800);
  await look('08-phone');
  const pressed = await press(`[...document.querySelectorAll('#tilo-page [data-a=back]')].find(e => e.offsetParent) ?? null`, 'the phone back');
  await sleep(900);
  step('phone back', {pressed, visibleRows: await tab.evaluate(`${rows}.filter(r => r.offsetParent).length`)});
  await look('09-phone-back');
  await sized(WIDE);

  // The workspace itself, which lists its apps, and each app a person opens
  // in their first hour: every one of them is the page the server renders.
  await openPage('home', '');
  const home = await look('11-home');
  if (home.refused) problems.push(`the workspace's own page was refused (${home.refused})`);
  const links = await tab.evaluate(`[...document.querySelectorAll('#tilo-page a[href^="/w/"]')].map(a => a.getAttribute('href'))`);
  step('home apps', {links});
  if (links.length < 5) problems.push(`the workspace's page offers ${links.length} apps`);
  await press(`document.querySelector('#tilo-page a[href$="/notes"]')`, 'Notes on the workspace page');
  if (await until(`location.pathname.endsWith('/notes') && document.querySelector('#tilo-page [data-p]')`, 30_000) === null) {
    problems.push('an app on the workspace page did not open');
  }
  await look('12-home-to-notes');
  for (const app of ['file-explorer', 'tasks', 'chat', 'mail', 'settings']) {
    await openPage(app, app);
    const seen = await look(`13-${app}`);
    if (seen.refused) problems.push(`${app}: refused (${seen.refused})`);
    else if (!seen.elements) problems.push(`${app}: shows nothing`);
  }

  // The appearance the person's browser asks for: a page follows it, as a
  // window follows the system it runs on.
  await openPage('light appearance');
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: 'light'}]});
  await sleep(900);
  await look('14-light');
  await tab.send('Emulation.setEmulatedMedia', {features: []});

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
