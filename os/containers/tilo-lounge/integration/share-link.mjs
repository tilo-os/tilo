// A shared link, opened: one person in the Lounge opens a document full tab,
// writes in it and copies its link from the bar's Share; another person,
// in a browser of their own on a phone-sized screen, opens that link and
// gets that one document full tab, with the first person's words and then
// their next ones as they write them, and the first person shown present in
// the bar. A link to an agent run opens the whole desktop with Console on
// that run's page: a run is read from the workspace's history, which the
// desktop's session serves.
//
// Both people come in through the Lounge's door as everyone does (the
// deployment admits everyone as an editor), so the link they share is the
// one the workspace's people open: no invitation rides in it.
//
// Screenshots of both pages, light and dark, at 390 and 1280 px wide, go
// under OUT_DIR for a person to look at.
//
// usage: node share-link.mjs SITE_URL WORKSPACE_ORIGIN WORKSPACE OUT_DIR
// Prints a JSON report; exits non-zero when the link did not open the same
// document live for the second person.
import {mkdirSync, writeFileSync} from 'node:fs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {stepLimit, machine} from './waits.mjs';

const [site, origin, workspace, out] = process.argv.slice(2);
if (!site || !origin || !workspace || !out) {
  console.error('usage: node share-link.mjs SITE_URL WORKSPACE_ORIGIN WORKSPACE OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});

// The document both people write in: Scratchpad's workspace document, one
// text that fills the window.
const APP = 'so.tilo.scratchpad';
const report = {site, origin, workspace, app: APP, at: new Date().toISOString(), shots: []};
const mark = clock();
const browsers = [];
const pages = [];

const finish = code => {
  writeFileSync(`${out}/report.json`, JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
  for (const browser of browsers) void browser.close?.();
  process.exit(code);
};
const failed = async message => {
  report.error = message;
  for (const {name, tab} of pages) {
    writeFileSync(`${out}/${name}.log`, (tab.log ?? []).join('\n'));
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(`${out}/${name}-failed.png`, shot);
    // What the page threw or called an error, wherever in its log it is, then
    // what it said last: a warning repeated on every frame pushes the one
    // error that stopped a page out of any last few lines.
    const log = tab.log ?? [];
    const errors = log.filter(line => /^(EXC|ERROR) /.test(line)).slice(-10);
    const repeated = new Map();
    for (const line of log) repeated.set(line, (repeated.get(line) ?? 0) + 1);
    const last = [...new Set(log.slice(-200).reverse())].slice(0, 30).reverse()
      .map(line => (repeated.get(line) > 1 ? `${line} (×${repeated.get(line)})` : line));
    if (errors.length) console.error(`--- ${name} threw:\n${errors.join('\n')}`);
    console.error(`--- ${name} said:\n${last.join('\n')}`);
  }
  console.error(`FAIL: ${message}`);
  console.error(`the machine as it failed: ${machine().text}`);
  finish(1);
};

// SwiftShader's WebGPU, as the Lounge's other browser checks run it.
const browserArgs = [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];

/** A person's own browser, at `width`×`height` CSS pixels. */
const person = async (name, {width, height, scale = 1, mobile = false}) => {
  const browser = await launch({args: browserArgs});
  browsers.push(browser);
  const tab = await browser.tab();
  // A file the page could not fetch is what stops it before it can say
  // why, so each one goes into the tab's log with what the browser said.
  const urls = new Map();
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') urls.set(params.requestId, params.request.url);
    if (method === 'Network.loadingFailed') tab.log.push(`LOAD ${urls.get(params.requestId)}: ${params.errorText}${params.blockedReason ? ` (${params.blockedReason})` : ''}`);
    if (method === 'Network.responseReceived' && params.response.status >= 400) tab.log.push(`LOAD ${params.response.url}: ${params.response.status}`);
  });
  await tab.send('Network.enable', {});
  await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: scale, mobile});
  if (mobile) await tab.send('Emulation.setTouchEmulationEnabled', {enabled: true}).catch(() => {});
  // Someone who has been here before: a browser new to the site opens on the
  // first screen instead of the Lounge.
  await tab.send('Page.addScriptToEvaluateOnNewDocument',
    {source: "try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}"});
  pages.push({name, tab});
  return tab;
};

/** Wait until `expression` holds in `tab`, or fail saying what. */
const until = async (tab, expression, what, timeout = stepLimit(4)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) {
      const shown = await tab.evaluate(`JSON.stringify({arrival: document.documentElement.dataset.arrival ?? null,
        entry: document.documentElement.dataset.entry ?? null,
        status: document.getElementById('status')?.textContent ?? null,
        text: (document.body.innerText || '').replace(/\\s+/g, ' ').slice(0, 300)})`).catch(() => 'unreadable');
      await failed(`${what} never happened: ${shown}`);
    }
    await sleep(250);
  }
};

/** Every name and value in the page's semantic tree, as one text. */
const shownText = tab => tab.evaluate(`(() => {
  globalThis.tiloMount?.requestSemantics?.();
  const out = [];
  const node = n => { for (const part of [n.name, n.value]) if (part) out.push(part); (n.children || []).forEach(node); };
  const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
  const root = globalThis.tiloMount?.state()?.semantics?.root;
  if (root) walk(root);
  return out.join('\\n');
})()`).catch(() => '');

/** Wait until `tab` shows `text`; resolves to how long that took. */
const shows = async (tab, text, what, timeout = stepLimit(3), required = true) => {
  const from = Date.now();
  for (;;) {
    if ((await shownText(tab)).includes(text)) return Date.now() - from;
    if (Date.now() - from > timeout) {
      if (!required) return null;
      await failed(`${what}: "${text}" never showed`);
    }
    await sleep(200);
  }
};

const type = async (tab, text) => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
  }
};

/** Screenshots of `tab` in light and dark as `name-<scheme>.png`. */
const shoot = async (tab, name) => {
  for (const scheme of ['light', 'dark']) {
    await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await sleep(1500);
    const shot = await tab.screenshot();
    if (shot) {
      writeFileSync(`${out}/${name}-${scheme}.png`, shot);
      report.shots.push(`${name}-${scheme}.png`);
    }
  }
  await tab.send('Emulation.setEmulatedMedia', {features: []});
};

/** Open `url` and wait for the page to have drawn what it opened. */
const arrive = async (tab, url, what) => {
  await tab.navigate(url);
  await until(tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, `${what} arriving`, stepLimit(6));
  const arrival = await tab.evaluate('document.documentElement.dataset.arrival');
  if (arrival !== 'done') await failed(`${what} stopped: ${await tab.evaluate("document.getElementById('status')?.textContent")}`);
  await until(tab, `document.documentElement.dataset.bar === 'shown'`, `${what}'s bar`);
};

// The workspace's host as a link names it: a loopback host reached over TLS
// keeps its scheme (`https:127.0.0.1:9781`), every other host is its name.
const host = new URL(origin);
const hostRoute = ['127.0.0.1', 'localhost', '[::1]'].includes(host.hostname) ? `https:${host.host}` : host.host;
const base = site.endsWith('/') ? site : `${site}/`;
const documentLink = `${base}app/#/@${hostRoute}/w/${workspace}/${APP}/-`;
report.opened = documentLink;

try {
  // 1. Ada opens the document full tab and writes in it.
  const ada = await person('ada', {width: 1280, height: 800});
  await arrive(ada, documentLink, 'Ada');
  mark('Ada opened the document full tab');
  const adaBar = JSON.parse(await ada.evaluate(`JSON.stringify({workspace: document.getElementById('bar-workspace').textContent,
    app: document.getElementById('bar-app').textContent, share: !document.getElementById('bar-share').hidden,
    tilo: document.getElementById('other-mode').href})`));
  report.adaBar = adaBar;
  if (!adaBar.share) await failed('Ada’s bar offers no Share in a workspace');
  if (!adaBar.tilo.includes(`/#/@${hostRoute}/w/${workspace}/${APP}`)) await failed(`Open in Tilo leads elsewhere: ${adaBar.tilo}`);
  // The document's text field: the editor publishes it without a place on
  // screen, so a person's click goes where it draws its first line, under
  // the window's header, as its placeholder shows.
  await until(ada, `(() => { globalThis.tiloMount?.requestSemantics?.(); return JSON.stringify(globalThis.tiloMount?.state()?.semantics ?? null).includes('MultilineTextInput'); })()`, 'the document’s text field', stepLimit(2));
  // What the app offers here and whether each is available now: an editor
  // that takes no keys says why in its own words.
  writeFileSync(`${out}/ada-actions.json`, await ada.evaluate(`JSON.stringify(globalThis.tiloMount?.actions?.() ?? null, null, 1)`).catch(error => String(error)));
  writeFileSync(`${out}/ada-state.json`, await ada.evaluate(`JSON.stringify((({semantics, ...rest}) => rest)(globalThis.tiloMount?.state?.() ?? {}), null, 1)`).catch(error => String(error)));
  // Where each press lands, for when the keys never reach the editor.
  await ada.evaluate(`globalThis.__presses = []; addEventListener('pointerdown', event => globalThis.__presses.push(
    \`\${document.elementsFromPoint(event.clientX, event.clientY).slice(0, 4).map(at => \`\${at.tagName}#\${at.id}.\${at.className}\`).join(' > ')} \${event.pointerType} arrival=\${document.documentElement.dataset.arrival}\`), true); true`);
  const focus = async () => {
    const canvas = JSON.parse(await ada.evaluate('JSON.stringify(globalThis.tiloMount.canvas.getBoundingClientRect())'));
    const x = canvas.x + Math.min(400, canvas.width / 2), y = canvas.y + 104;
    await ada.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased']) {
      await ada.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
    }
    await sleep(500);
  };
  // A person writes once the page lets them: the workspace's answer to what
  // they may do here arrives just after the page opens.
  await until(ada, `(globalThis.tiloMount?.actions?.() ?? []).some(action => action.address?.action === 'field-edit' && action.contract?.availability?.state === 'available')`,
    'the document taking edits', stepLimit(2));
  // Writes `text` as Ada's next words: from the keyboard, as she types; if
  // the keys never reach the editor, through the app's own field edit (what
  // its controls and an agent use), so the rest still runs, and the run
  // fails at its end on the keyboard alone.
  let written = '';
  const write = async text => {
    const before = await ada.evaluate('globalThis.tiloMount?.state()?.frames ?? null');
    await focus();
    // The click put the caret where it landed: the words go after the rest.
    for (const type of ['rawKeyDown', 'keyUp']) {
      await ada.send('Input.dispatchKeyEvent', {type, key: 'End', code: 'End', windowsVirtualKeyCode: 35, modifiers: 2});
    }
    await type(ada, text);
    written += text;
    const typed = await shows(ada, text.trim(), 'Ada’s typed words', stepLimit(1), false);
    if (typed !== null) return;
    report.keyboard = {
      reached: false,
      // What the click landed on, and where the page's input went.
      under: await ada.evaluate(`(() => { const c = globalThis.tiloMount.canvas.getBoundingClientRect();
        const at = document.elementFromPoint(c.x + Math.min(400, c.width / 2), c.y + 104);
        return at ? \`\${at.tagName}#\${at.id}.\${at.className}\` : null; })()`),
      lastInput: await ada.evaluate('globalThis.tiloMount?.state()?.lastInput ?? null'),
      framesBefore: before,
      framesAfter: await ada.evaluate('globalThis.tiloMount?.state()?.frames ?? null'),
      focused: await ada.evaluate('`${document.activeElement?.tagName}.${document.activeElement?.className}`'),
      presses: await ada.evaluate('globalThis.__presses ?? null'),
      phase: await ada.evaluate('globalThis.tiloMount?.state()?.phase ?? null'),
    };
    const answer = await ada.evaluate(`JSON.stringify(globalThis.tiloMount.invokeAction('scratchpad.note', 'field-edit',
      {edit: {path: [{field: 'body'}], op: 'set', value: ${JSON.stringify(written)}}}, {confirm: true}))`);
    report.keyboard.edited = JSON.parse(answer ?? 'null');
    await shows(ada, text.trim(), 'Ada’s words written through the field edit');
  };
  const first = `Shared from Ada ${Date.now().toString(36)}`;
  await write(first);

  // 2. Share, then Copy link: the clipboard the page writes is the link.
  await ada.evaluate(`navigator.clipboard.writeText = text => { globalThis.__copied = text; return Promise.resolve(); }`);
  await ada.evaluate(`document.getElementById('bar-share').click()`);
  await until(ada, `Boolean(document.querySelector('.bar-sheet'))`, 'the Share sheet');
  await shoot(ada, 'share-sheet-1280');
  await ada.evaluate(`document.querySelector('.bar-sheet button.bar-primary').click()`);
  const link = await until(ada, 'globalThis.__copied', 'the copied link', stepLimit(1));
  report.copied = link;
  if (!link.startsWith(`${base}app/#/`) || !link.includes(`/w/`) || !link.includes(`/${APP}/`) || /[?&]invite=/.test(link)) {
    await failed(`Share copied ${link}, not this document's link for the workspace's people`);
  }
  await ada.send('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
  await ada.send('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
  mark('Ada wrote in it and copied its link');

  // 3. Bo opens it on a phone: the same document, full tab, Ada's words in it.
  const bo = await person('bo', {width: 390, height: 844, scale: 2, mobile: true});
  const opening = Date.now();
  await arrive(bo, link, 'Bo');
  report.boOpenedMs = Date.now() - opening;
  report.boTarget = JSON.parse(await bo.evaluate('JSON.stringify(globalThis.tiloRouter?.route?.target ?? null)'));
  if (report.boTarget?.app !== APP) await failed(`Bo’s page shows ${report.boTarget?.app}, not the shared document`);
  if (await bo.evaluate('Boolean(globalThis.tiloMount?.navigation)')) await failed('Bo’s link opened a desktop, not the one document');
  report.boSawAdaMs = await shows(bo, first, 'Ada’s words on Bo’s page');
  mark('Bo opened the link on a phone and read Ada’s words');

  // 4. Ada writes on; Bo sees it as she writes.
  const next = ` and then ${Date.now().toString(36)}`;
  const typed = Date.now();
  await write(next);
  await shows(bo, next.trim(), 'Ada’s next words on Bo’s page');
  report.liveEditMs = Date.now() - typed;

  // 5. Each sees the other here, in this document.
  await until(bo, `document.querySelectorAll('#bar-people .bar-face:not(.bar-elsewhere)').length > 0`, 'Ada in Bo’s bar', stepLimit(4));
  await until(ada, `document.querySelectorAll('#bar-people .bar-face:not(.bar-elsewhere)').length > 0`, 'Bo in Ada’s bar', stepLimit(4));
  report.boBar = JSON.parse(await bo.evaluate(`JSON.stringify([...document.querySelectorAll('#bar-people .bar-face')].map(face => face.getAttribute('aria-label')))`));
  await shoot(bo, 'document-390');
  await shoot(ada, 'document-1280');
  await bo.evaluate(`document.getElementById('bar-share').click()`);
  await until(bo, `Boolean(document.querySelector('.bar-sheet'))`, 'Bo’s Share sheet');
  await shoot(bo, 'share-sheet-390');
  mark('Bo read Ada’s next words live and saw her here');

  // 6. A link to an agent run opens the desktop, Console on that run's page.
  const run = `console.run:${process.env.SHARE_LINK_RUN ?? 'no-such-run'}`;
  const runLink = `${base}#/@${hostRoute}/w/${workspace}/so.tilo.console/-/${run}`;
  report.runLink = runLink;
  // In a browser of its own: a link is opened fresh, not by editing the
  // address of a page already open on another app.
  const cy = await person('cy', {width: 390, height: 844, scale: 2, mobile: true});
  await cy.navigate(runLink);
  await until(cy, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'Cy at a run arriving', stepLimit(6));
  if (await cy.evaluate('document.documentElement.dataset.arrival') !== 'done') {
    await failed(`Cy at a run stopped: ${await cy.evaluate("document.getElementById('status')?.textContent")}`);
  }
  await until(cy, `globalThis.tiloMount?.state()?.livePlace?.focus?.app === 'so.tilo.console'`, 'Console focused on the desktop', stepLimit(4));
  report.runPlace = JSON.parse(await cy.evaluate('JSON.stringify(globalThis.tiloMount?.state()?.livePlace?.focus ?? null)'));
  report.runTarget = JSON.parse(await cy.evaluate('JSON.stringify(globalThis.tiloRouter?.route?.target ?? null)'));
  if (report.runTarget?.app !== 'so.tilo.console' || report.runTarget?.resource?.id !== run) {
    await failed(`the run link opened ${JSON.stringify(report.runTarget)}`);
  }
  await sleep(3000);
  const said = await cy.evaluate(`document.getElementById('status')?.textContent ?? ''`);
  if (/not there|cannot open/.test(said)) await failed(`the run's view did not open: ${said}`);
  report.runShows = (await shownText(cy)).slice(0, 400);
  await shoot(cy, 'run-390');
  mark('a run link opened Console on that run');
  if (report.keyboard?.reached === false) {
    report.error = 'Ada’s keys never reached the editor of a document opened full tab';
    finish(1);
  }
  report.ok = true;
  finish(0);
} catch (error) {
  await failed(error?.stack ?? String(error));
}
