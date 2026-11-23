// A stranger's way into tilo.sh, in real browsers, from the first visit to a
// workspace of their own: what the site says to someone it does not know yet,
// what the mail they get opens, what the person who runs the host does about
// it, and where the person ends up once they are in.
//
// The story this drives, one step per mark below:
//
//   0. the person who runs the host arrives, and the stack's own owner adds
//      them as an owner with `tilo do workspace/members#admit` — how a fresh
//      invitation-only Lounge gets its first member; the entry lets them in,
//      and the workspace makes them an owner once they are;
//   1. a stranger arrives: the entry's wait list, not the setup screen, and an
//      address left on it is confirmed through the mail the entry sends;
//   2. the operator hands an invite link over in one press, and invites that
//      address from the workspace's own wait list, which mails the invitation;
//   3. the stranger follows the invitation's link and joins; joining founds
//      their person, so the entry shows their 24 recovery words once — this
//      reads them from the card and checks they restore the same person — and
//      the workspace opens, where the setup offered inside it sets Tilo up
//      through their agent (`tilo login`, then the agent adds Tilo's MCP
//      server) in that workspace;
//   3b. the same person on a browser new to the site signs in with their
//      agent: the entry's wait list offers it, and the computer `tilo login`
//      set up is a device of theirs, which signs the new tab in;
//   4. they make a second workspace and are in it;
//   5. someone else with no invitation still cannot get in, and leaves an
//      address on the wait list.
//
// The mails are the entry's own, kept by the mail sink the stack points it at:
// the run reads each one's link out of the file it landed in, so what the flow
// proves is the mail a person would get.
//
// usage: node onboarding.mjs SITE SERVER WORK TILO_BIN STACK
//
// SITE is the web entry's origin (the site people visit), SERVER the workspace
// host's, WORK the run's directory (the stack's state, the mail sink's files
// and every page's screenshots and console live there), TILO_BIN the `tilo`
// command an agent runs, and STACK os/containers/tilo-sh/stack.sh of the stack
// the Lounge runs on: its own owner adds the operator as an owner with
// `stack.sh owner KEY`. The stack is plain http on this machine, so no
// certificate stands between the browsers, the agent's command and it.
import {execFile, execFileSync, spawn} from 'node:child_process';
import {chmodSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {FIRST_SCREEN_SEEN, launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {stepLimit, shellMetrics, machine, browserMemory, confirmRecoveryWords} from './waits.mjs';
import {mailLink, poll, pressText, waitForMail, waitingCard} from './shell.mjs';

const [site, server, work, tiloBin, stack] = process.argv.slice(2);
if (!site || !server || !work || !tiloBin || !stack) {
  console.error('usage: node onboarding.mjs SITE SERVER WORK TILO_BIN STACK');
  process.exit(2);
}
// The mail sink the stack points the entry at (`stack.sh mail` reads the same).
const mailDir = join(work, 'state', 'mail');
const computer = join(work, 'computer');
mkdirSync(mailDir, {recursive: true});
const mark = clock();
const report = {site, server, at: new Date().toISOString(), steps: []};
const browsers = [];
const said = [];
/** The part of the flow under way. Every line this driver prints names a part
 * that has finished, so what stopped inside one has nothing else to say which:
 * a failure, and a flow that ends without failing, name this. */
let doing = 'starting';
const step = name => { doing = name; };

/** Where the host is as the pages know it: a workspace is stored per origin. */
const hostOrigin = new URL(server).origin;
/** The site's own origin: what its storage belongs to. */
const siteOrigin = new URL(site).origin;

// A person's tabs share one browser and the flow reads the one behind: the
// second tab must be let in without the first losing the workspace. The
// browser's baseline leaves every tab answering as the front one does
// (`launch` in tools/web/chrome.mjs).

const pageText = tab => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 1200)`)
  .catch(() => '');
const state = tab => tab.evaluate(`JSON.stringify({
  arrival: document.documentElement.dataset.arrival ?? null,
  refusal: document.documentElement.dataset.refusal ?? null,
  waitList: document.documentElement.dataset.waitList ?? null,
  entry: document.documentElement.dataset.entry ?? null,
  firstScreen: document.documentElement.dataset.firstScreen ?? null,
  title: document.title,
  workspace: document.documentElement.dataset.workspace ?? null,
})`).then(JSON.parse).catch(error => ({unreachable: String(error?.message ?? error).slice(0, 300)}));

const dump = async (tab, name) => {
  const png = await Promise.race([tab.screenshot().catch(() => null), sleep(stepLimit(2)).then(() => null)]);
  if (png) writeFileSync(join(work, `${name}.png`), png);
  writeFileSync(join(work, `${name}.log`), (tab.log ?? []).join('\n'));
};

const fail = async message => {
  if (!report.error) report.error = message;
  console.error(`FAIL: ${message}`);
  console.error(`it failed while ${doing}`);
  console.error(`the machine as it failed: ${machine().text}`);
  for (const {name, tab} of said) {
    const lines = (tab.log ?? []).slice(-40);
    if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
  }
  // A tab that stopped answering (its renderer gone, a dialog up) answers
  // none of these: each is given a step's time, not the flow's.
  const bounded = (work, otherwise) => Promise.race([work.catch(() => otherwise), sleep(stepLimit(1)).then(() => otherwise)]);
  for (const {name, tab} of said) {
    await bounded(dump(tab, `failed-${name}`), undefined);
    report[`${name}Text`] = await bounded(pageText(tab), '(the tab did not answer)');
    report[`${name}State`] = await bounded(state(tab), {});
  }
  writeFileSync(join(work, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  process.exit(1);
};
const finished = async () => {
  report.finished = true;
  for (const {name, tab} of said) await dump(tab, name).catch(() => {});
  writeFileSync(join(work, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  console.log(JSON.stringify(report, null, 2));
  process.exit(0);
};
// A flow that ends without a part failing it: the process has nothing left to
// do (a browser command nothing will ever answer leaves the top-level await
// unsettled, and node exits with a bare warning that names no part and no
// command), or something kills it. Say which part was under way and what the
// browsers still owed, so the wrapper's one line is not the whole report.
process.on('exit', code => {
  if (report.finished || report.error) return;
  const owed = browsers.flatMap(browser => browser.outstanding?.() ?? []);
  console.error(`FAIL: the flow ended while ${doing} (node exited ${code})`);
  console.error(`what the browsers still owed: ${owed.length ? owed.join(', ') : 'nothing'}`);
  for (const {name, tab} of said) {
    const lines = (tab.log ?? []).slice(-20);
    if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
  }
  report.ended = {while: doing, owed, exit: code};
  try { writeFileSync(join(work, 'report.json'), JSON.stringify(report, null, 2)); } catch {}
});

const failures = tab => (tab.log ?? []).filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));
const checkPage = async (tab, name) => {
  const found = failures(tab);
  if (found.length) {
    report[`${name}State`] = await state(tab);
    await fail(`the page failed while ${name}: ${found.slice(0, 5).join(' | ')}`);
  }
};
/** Wait until the page answers `expression` true, or say what it was doing.
 * The page does the waiting: it reads the expression again on each change it
 * shows, and the wait carries on in the next document after a navigation. */
const until = async (tab, expression, what, timeout = stepLimit(4)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  // A tab that stopped answering (its renderer gone, a dialog up) answers
  // none of these: each is given a step's time, not the flow's.
  const answered = work => Promise.race([work, sleep(stepLimit(1)).then(() => null)]);
  const seen = await answered(state(tab));
  const text = await answered(pageText(tab));
  const cards = await answered(tab.evaluate(`[...document.querySelectorAll('.tilo-entry-card, [data-card]')].map(card => card.innerText.trim()).filter(Boolean).join(' | ')`).catch(() => ''));
  await fail(`${what} never happened: ${seen === null ? 'the tab did not answer' : JSON.stringify(seen)} — "${(text ?? '').slice(0, 400)}"${cards ? ` — on screen: ${cards.slice(0, 600)}` : ''}`);
};
/** A page of the person's own: a browser of its own, and a tab that has not
 * been to the site before, so the site treats it as a first visit. A person
 * who has been here before (`setupDone`) opens the site itself, not its first
 * screen: the workspace's own desktop, as the people who run it see it. */
const visit = async (name, {setupDone = false} = {}) => {
  const browser = await launch();
  browsers.push(browser);
  const tab = await browser.tab({firstScreen: true});
  tab.log ??= [];
  if (setupDone) await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FIRST_SCREEN_SEEN});
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Log.enable', {}).catch(() => {});
  await tab.send('Page.enable', {}).catch(() => {});
  await tab.send('Inspector.enable', {}).catch(() => {});
  const asked = new Map();
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
    else if (method === 'Network.responseReceived' && params.response.status >= 400) tab.log.push(`HTTP ${params.response.status} ${params.response.url}`);
    else if (method === 'Log.entryAdded') tab.log.push(`LOG ${params.entry.level} ${params.entry.text}`);
    // Where the tab went, and why it stopped answering: a page that never
    // came after a navigation names one of these.
    else if (method === 'Page.frameNavigated' && !params.frame.parentId) tab.log.push(`PAGE now at ${params.frame.url.replace(/#.*/, '#…')}`);
    else if (method === 'Page.javascriptDialogOpening') tab.log.push(`PAGE dialog ${params.type}: ${params.message}`);
    else if (method === 'Inspector.targetCrashed') {
      tab.log.push(`PAGE the renderer crashed; the browser said: ${browser.crashLines?.() || '(nothing)'}`);
      tab.log.push(`PAGE memory at the crash: ${browserMemory()}`);
      // The browser's own report of how it ended (a signal, or memory) comes
      // a moment after the page's.
      setTimeout(() => tab.log.push(`PAGE why: ${tab.crash?.() ?? 'the browser gave no reason'}`), 1_500).unref?.();
    }
    else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 500));
    }
  });
  await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics).catch(() => {});
  said.push({name, tab});
  return {name, tab, browser};
};
/** Who this page is, and which workspace it holds. */
const person = tab => tab.evaluate(`JSON.stringify({
  user: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.userPublicKey ?? null; } catch { return null; } })(),
  workspace: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null')?.workspace ?? null; } catch { return null; } })(),
  title: document.title,
})`).then(JSON.parse).catch(() => ({}));
/** The person's own workspace, as the page titles it: never the Lounge's. */
const ownWorkspace = tab => tab.evaluate(`/^Tilo · /.test(document.title) && !/lounge/i.test(document.title) && document.title`).catch(() => '');
/** The person's own workspace, drawn, as the page publishes it: arrived, its
 * page drawn, named after a workspace that is not the Lounge. */
const OWN_WORKSPACE = `(() => {
  const name = document.documentElement.dataset.workspace ?? '';
  const drawn = document.documentElement.dataset.arrival === 'done'
    && !!document.querySelector('#tilo-page [data-p]');
  return drawn && name && !/lounge/i.test(name) ? name : '';
})()`;

// -- the page's own controls --------------------------------------------------
const clickAt = async (tab, x, y) => {
  await Promise.all([
    tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}),
    ...['mousePressed', 'mouseReleased'].map(type => tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1})),
  ]);
  // The press is the page's to take on its next frame, and what it opened is
  // read after this (the waits themselves end on the page's changes).
  await sleep(400);
};
const type = async (tab, text) => {
  await Promise.all([...text].flatMap(ch => [
    tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch}),
    tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch}),
  ]));
  // The shell reads the query on its next frame; the wait that follows is on
  // its own answer to it.
  await sleep(600);
};
/** A person's search on the page the server renders: the workspace's own
 * search, at the address that names it, typed into and its row titled `title`
 * pressed — an app, a record, or one of the workspace's actions, which the
 * server itself runs. */
const pageSearch = async (tab, query, title) => {
  const workspace = await until(tab,
    `location.pathname.startsWith('/w/') ? location.pathname.split('/')[2] : ''`,
    'the page naming its workspace in its address', stepLimit(4));
  await tab.navigate(`${siteOrigin}/w/${workspace}/search`);
  await until(tab, `!!document.querySelector('#tilo-page .tilo-search-field')`, 'the search page opening', stepLimit(6));
  const field = `document.querySelector('#tilo-page .tilo-search-field')`;
  await until(tab, `document.activeElement === ${field}`, 'the search field taking the caret', stepLimit(3));
  await type(tab, query);
  const row = `[...document.querySelectorAll('#tilo-page .tilo-search-row')]
    .find(row => (row.innerText || '').includes(${JSON.stringify(title)})) ?? null`;
  const rows = JSON.parse(await until(tab, `(() => {
    const rows = [...document.querySelectorAll('#tilo-page .tilo-search-row')].map(row => (row.innerText || '').replace(/\s+/g, ' ').trim());
    return rows.some(text => text.includes(${JSON.stringify(title)})) ? JSON.stringify(rows) : '';
  })()`, `the search offering "${title}"`, stepLimit(4)));
  const box = JSON.parse(await until(tab, `(() => {
    const found = (${row});
    if (!found) return '';
    found.scrollIntoView({block: 'nearest'});
    const rect = found.getBoundingClientRect();
    return rect.width && rect.height
      ? JSON.stringify({x: rect.x + Math.min(rect.width / 2, 40), y: rect.y + rect.height / 2}) : '';
  })()`, `the row "${title}" drawn`, stepLimit(3)));
  await clickAt(tab, box.x, box.y);
  return rows;
};
/** A person's search: the page's own, typed into, its row titled `title`
 * pressed. */
const search = (tab, query, title) => pageSearch(tab, query, title);
/** A real pointer press on the control the page draws named `name`, once it
 * is drawn. */
const pressName = async (tab, name, timeout = stepLimit(3)) => {
  const shown = await poll(tab, `(() => {
    const page = document.getElementById('tilo-page');
    const found = [];
    for (const el of page?.querySelectorAll('[aria-label], button, a') ?? []) {
      const box = el.getBoundingClientRect();
      if (!box.width || !box.height) continue;
      found.push({name: el.getAttribute('aria-label') ?? (el.innerText || '').trim().split('\n')[0],
        bounds: {x0: box.x, y0: box.y, x1: box.right, y1: box.bottom}});
    }
    const hit = found.find(n => n.name === ${JSON.stringify(name)} && n.bounds);
    if (!hit) return '';
    const b = hit.bounds;
    return JSON.stringify({x: Math.round((b.x0 + b.x1) / 2), y: Math.round((b.y0 + b.y1) / 2)});
  })()`, timeout);
  if (!shown) return false;
  const box = JSON.parse(shown);
  // What a press there reaches: the page's canvas, or something of the page over it.
  const under = await tab.evaluate(`(() => { const e = document.elementFromPoint(${box.x}, ${box.y});
    return e ? e.localName + (e.id ? '#' + e.id : '') + (e.className ? '.' + String(e.className).split(' ')[0] : '') : 'nothing'; })()`).catch(() => '?');
  console.log(`press ${JSON.stringify(name)} at ${box.x},${box.y} on ${under}`);
  await clickAt(tab, box.x, box.y);
  return true;
};
/** A click on the DOM control `id`, as a person's own press on it. */
const pressId = async (tab, id) => {
  const box = JSON.parse(await until(tab, `(() => {
    const element = document.getElementById(${JSON.stringify(id)});
    if (!element || element.disabled) return '';
    const rect = element.getBoundingClientRect();
    if (!rect.width || !rect.height) return '';
    return JSON.stringify({x: rect.x + rect.width / 2, y: rect.y + rect.height / 2});
  })()`, `the control #${id}`));
  await clickAt(tab, box.x, box.y);
};
// -- the mails ---------------------------------------------------------------
/** The `count`th mail the entry sent, or the failure saying what did not
 * arrive: the sink keeps each as a file, and `mailLink` reads a link out of it. */
const mailArrived = (what, count) => waitForMail(mailDir, what, count)
  .catch(error => fail(error.message));

/** Whether the first screen is in front of the person. Its element is in every
 * page the entry serves (`#first-screen`), so what shows is whether the page set
 * `data-first-screen` and displayed it. */
const firstScreenShown = `(document.documentElement.dataset.firstScreen !== undefined
  && getComputedStyle(document.getElementById('first-screen')).display !== 'none')`;

// -- tilo.sh as a person meets it --------------------------------------------
/** A person arriving at the site, in a tab that has not been here before.
 * `expected: 'let in'` tries again while the entry refuses: a person made an
 * owner just now may meet the closed entry once more before it opens for them,
 * and comes back — as the operator does — instead of landing on the wait list. */
const arrive = async (page, {expected}) => {
  const deadline = Date.now() + stepLimit(6);
  // What the page reaches first: a decision on the entry, or — once it lets the
  // person in — the 24 recovery words it shows them before anything opens.
  const arrivedOrWords = `(() => {
    const root = document.documentElement;
    if (['done', 'stopped'].includes(root.dataset.arrival)) return 'settled';
    return document.querySelector('[data-card="recovery-words"]') ? 'words' : '';
  })()`;
  for (;;) {
    await page.tab.navigate(site);
    let settledBy = Date.now() + stepLimit(6);
    for (;;) {
      const stage = await page.tab.waitFor(arrivedOrWords, {timeout: Math.max(1000, settledBy - Date.now())}).catch(() => '');
      if (stage === 'words') {
        await keepRecoveryWords(page);
        // The card is kept: the page goes on to open the workspace.
        settledBy = Date.now() + stepLimit(6);
        continue;
      }
      if (stage === 'settled') break;
      await fail(`${page.name} arriving never happened: ${JSON.stringify(await state(page.tab))} — "${(await pageText(page.tab)).slice(0, 300)}"`);
    }
    const seen = await state(page.tab);
    report.steps.push({step: `${page.name} arrived`, ...seen});
    if (seen.arrival === 'done' || expected === 'refused') return seen;
    if (Date.now() > deadline) await fail(`${page.name} did not arrive: ${JSON.stringify(seen)} — "${(await pageText(page.tab)).slice(0, 300)}"`);
    await sleep(500);
  }
};

// -- the deployment's owners admit people ------------------------------------
/** Make `user` (hex) an owner of the Lounge, as the stack's own owner does:
 * `stack.sh owner KEY` runs `tilo do workspace/members#admit person=KEY role=owner`, the action
 * any owner takes, and the workspace's entry lets them in. */
const addOwner = async user => {
  execFileSync('bash', [stack, 'owner', user], {stdio: ['ignore', 'inherit', 'inherit']});
};

// The agent's computer: its own home, the MCP configuration the agent keeps
// there, and the test certificate the command trusts for this run's site.
mkdirSync(join(computer, 'bin'), {recursive: true});
const agentConfiguration = join(computer, 'agent-mcp.json');
const agentEnv = async () => ({
  PATH: `${join(computer, 'bin')}:${process.env.PATH}`,
  HOME: computer,
  XDG_STATE_HOME: join(computer, 'state'),
  XDG_CONFIG_HOME: join(computer, 'config'),
});
const tilo = async (env, args) => new Promise(resolve => {
  execFile(tiloBin, args, {env, timeout: 180_000, maxBuffer: 1 << 20},
    (error, stdout, stderr) => resolve({code: error ? (error.code ?? 1) : 0, stdout, stderr}));
});
/** What the first screen copies, read from the clipboard: a person's click
 * writes it inside the gesture, and the browser lands the write a moment
 * later, so a prompt an earlier copy left there is not this one. */
const copied = async (tab, before) => {
  const read = `navigator.clipboard.readText().then(text =>
    text.match(/(?:tilo|sh -s) (?:login|device add) /) && text !== ${JSON.stringify(before)} ? text : '', () => '')`;
  // The clipboard is the browser's, not the page's: a write landing is no
  // change the page tells anyone about, so this is the one wait here that
  // reads on a clock, and it holds a few steps.
  for (const deadline = Date.now() + stepLimit(4); Date.now() < deadline;) {
    const text = await tab.evaluate(read).catch(() => '');
    if (text) return text;
    await sleep(200);
  }
  await fail('the prompt never reached the clipboard');
};
const commandOf = prompt => {
  const login = prompt.match(/(?:tilo|sh -s) login (\S+)/);
  const add = prompt.match(/(?:tilo|sh -s) device add (\S+)/);
  const text = login?.[1] ?? add?.[1];
  if (!text) return null;
  return {text, kind: add ? 'device add' : 'login',
    ticket: /^tilo1[0-9a-f]+$/.test(text) ? text : null,
    code: /^[2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4}$/.test(text) ? text : null,
    site: prompt.match(/--site (\S+?)(?:,|\s|$)/)?.[1] ?? null};
};
/** The page's own identity module: the one it holds its person in. */
const identityModule = `await import(new URL('identity.mjs', new URL('.', (document.querySelector('link[rel="modulepreload"][href], script[type="module"][src]').href
  || document.querySelector('script[type="module"][src]').src))).href)`;

/** The 24 recovery words, shown once right after a browser's person is first
 * let in, and confirmed before anything else opens. The words are read from
 * the card the person sees, checked to be 24 words, and derived again to that
 * person's key: the same phrase has to restore the same root. Then they say
 * they kept them, and the page goes on. */
const keepRecoveryWords = async page => {
  const shown = JSON.parse(await until(page.tab, `(() => {
    const card = document.querySelector('[data-card="recovery-words"]');
    if (!card) return '';
    return JSON.stringify([...card.querySelectorAll('.tilo-recovery-word')]
      .map(item => (item.querySelector('.tilo-recovery-text')?.textContent ?? '').trim()));
  })()`, `${page.name} shown the 24 recovery words`, stepLimit(10)));
  if (shown.length !== 24) await fail(`the recovery card shows ${shown.length} words, not 24: ${shown.join(' ')}`);
  if (!shown.every(word => /^[a-z]+$/.test(word))) await fail(`the recovery card carries something other than words: ${shown.join(' ')}`);
  const restored = await page.tab.evaluate(`(async () => {
    const id = ${identityModule};
    return id.recoveryWordsPublicKey(${JSON.stringify(shown.join(' '))});
  })()`).catch(error => fail(`the 24 words did not read back: ${error?.message ?? error}`));
  const person = await page.tab.evaluate(`(async () => {
    const id = ${identityModule};
    return (await id.browserIdentity({create: false}))?.userPublicKey ?? null;
  })()`).catch(() => null);
  if (!person) await fail(`${page.name} keeps no person the 24 words could restore`);
  if (restored !== person) await fail(`the 24 words restore ${restored}, not ${page.name}'s person (${person})`);
  // The card in both themes, while it is up, before it is confirmed away.
  await page.tab.evaluate(`document.documentElement.dataset.look = 'light'`);
  await dump(page.tab, `recovery-words-${page.name}-light`);
  await page.tab.evaluate(`document.documentElement.dataset.look = 'dark'`);
  await dump(page.tab, `recovery-words-${page.name}-dark`);
  await page.tab.evaluate(`delete document.documentElement.dataset.look`);
  console.log(`ok: ${page.name} was shown the 24 recovery words, which restore their person (${person.slice(0, 12)}…)`);
  report.recovery = {...report.recovery, [page.name]: {words: shown.length, restores: person, screenshots: [`recovery-words-${page.name}-light.png`, `recovery-words-${page.name}-dark.png`]}};
  mark(`${page.name} was shown their 24 recovery words, which restore their person`);
  // Kept, as a person presses it; the shared press every other founder uses.
  await confirmRecoveryWords(page.tab);
  await until(page.tab, `!document.querySelector('[data-card="recovery-words"]')`, `${page.name}'s words card closing`, stepLimit(4));
};

try {
  // -- 0. the operator, made an owner by the stack, is let in --------------
  // A fresh invitation-only Lounge has nobody in it: the people the
  // deployment's owners add are the ones its entry lets in, so the person who
  // runs it arrives first, and this run adds their key as an owner with
  // `stack.sh owner`, which runs `tilo do workspace/members#admit`.
  step('the operator the deployment made an owner being let in');
  const operator = await visit('operator', {setupDone: true});
  await arrive(operator, {expected: 'refused'});
  const operatorPerson = await operator.tab.evaluate(`(async () => {
    const id = ${identityModule};
    return (await id.browserIdentity()).userPublicKey;
  })()`).catch(() => null);
  if (!operatorPerson) await fail('the operator\'s tab made no person to add');
  await addOwner(operatorPerson);
  await arrive(operator, {expected: 'let in'});
  await until(operator.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.workspace`,
    'the entry letting in the person the deployment made an owner', stepLimit(8));
  mark('the operator the deployment made an owner is let in by the closed entry');

  // The operator gives their name, as anyone does: the invitations they make
  // say who asked the stranger in, and the first thing that stranger reads is
  // a person, never "someone".
  step('the operator giving their name, so those they invite know who asked them in');
  const operatorName = 'Maya Chen';
  await search(operator.tab, 'set your name', 'Set your name');
  await until(operator.tab, `!!document.querySelector('.tilo-search-form input[name=name]')`, 'the name form', stepLimit(4));
  await operator.tab.evaluate(`(() => {
    const input = document.querySelector('.tilo-search-form input[name=name]');
    input.value = ${JSON.stringify(operatorName)};
    input.dispatchEvent(new Event('input', {bubbles: true}));
    input.closest('form').querySelector('button[type=submit]').click();
  })()`);
  // The action answers on the page: done, or why it was not.
  const nameSaid = await until(operator.tab, `(() => {
    if (document.querySelector('.tilo-search-form input[name=name]')) return '';
    const said = document.querySelector('#tilo-page .tilo-search-notice')?.textContent?.trim() ?? '';
    return said.startsWith('Running ') ? '' : said;
  })()`, 'the name kept', stepLimit(6));
  if (nameSaid !== 'Set your name: Done.') await fail(`setting the operator's name answered "${nameSaid}"`);
  report.operator = {user: operatorPerson, name: operatorName};
  mark('the operator gave their name, so those they invite know who asked them in');

  // -- 1. a stranger at the wait list ----------------------------------------
  step('a stranger arriving at the wait list');
  const stranger = await visit('stranger');
  await arrive(stranger, {expected: 'refused'});
  await until(stranger.tab, `document.documentElement.dataset.refusal === 'wait-list'`, "the entry showing its wait list", stepLimit(6));
  await until(stranger.tab, `document.documentElement.dataset.waitList === 'form'`, 'the wait list offering its field');
  await until(stranger.tab, `!${firstScreenShown}`, 'the setup screen leaving the entry');
  report.waitList = await stranger.tab.evaluate(`({
    heading: document.getElementById('arrival-refusal-heading')?.textContent?.trim() ?? null,
    body: document.getElementById('arrival-refusal-body')?.textContent?.trim() ?? null,
    note: document.getElementById('arrival-wait-note')?.textContent?.trim() ?? null,
    button: document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null,
    signIn: document.getElementById('arrival-sign-in-agent')?.textContent?.trim() ?? null,
    contact: document.getElementById('arrival-wait-contact')?.textContent?.trim() ?? null,
  })`);
  await dump(stranger.tab, '1-wait-list');
  console.log(`ok: a stranger met the wait list, not the setup screen (${report.waitList.heading})`);
  mark('a stranger met the wait list');

  step('the stranger leaving an address and confirming it through the mail');
  const address = 'ada@example.org';
  await stranger.tab.evaluate(`(() => {
    const field = document.getElementById('arrival-wait-email');
    field.value = ${JSON.stringify(address)};
    field.dispatchEvent(new Event('input', {bubbles: true}));
    document.querySelector('#arrival-wait button').click();
  })()`);
  await until(stranger.tab, `document.documentElement.dataset.waitList === 'inbox'`, 'the address being left', stepLimit(4));
  report.inbox = await stranger.tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
  const confirmation = await mailArrived('the confirmation mail', 1);
  report.confirmationMail = {to: confirmation.to, from: confirmation.from, subject: confirmation.subject};
  const confirmLink = mailLink(confirmation, 'wait-list-confirm');
  if (!confirmLink) await fail(`the confirmation mail carries no confirmation link: ${(confirmation.text ?? '').slice(0, 400)}`);
  console.log(`ok: the confirmation mail reached ${confirmation.to} (${confirmation.subject})`);
  await stranger.tab.navigate(confirmLink);
  await until(stranger.tab, `document.documentElement.dataset.waitList === 'listed'`, 'the confirmation link listing the address', stepLimit(6));
  report.listed = await stranger.tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
  await dump(stranger.tab, '2-on-the-list');
  mark('the stranger left an address and confirmed it through the mail');

  // -- 2. the operator hands over links: one press for someone with no
  //      address, then the address on the wait list ------------------------
  // And a link for someone the operator has no address for: one press, and it
  // is on the clipboard, with no sheet and no role to choose.
  step('the operator handing over an invite link and inviting the address');
  await operator.tab.send('Browser.grantPermissions', {origin: siteOrigin,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  // Only the tab in front may read the clipboard of a browser here: the
  // operator's is one of several.
  await operator.tab.front();
  await operator.tab.send('Emulation.setFocusEmulationEnabled', {enabled: true}).catch(() => {});
  report.clipboardFocused = await operator.tab.evaluate('document.hasFocus()').catch(() => null);
  const clipboardBefore = await operator.tab.evaluate(`navigator.clipboard.readText().catch(() => '')`).catch(() => '');
  await search(operator.tab, 'copy invite', 'Copy an invite link');
  const handed = await poll(operator.tab,
    `(() => {
      const shared = document.querySelector('[data-card=share] input[name=link]')?.value;
      if (shared && shared !== ${JSON.stringify(clipboardBefore)}) return 'shared:' + shared;
      return navigator.clipboard.readText().then(text => /\\?invite=[A-Za-z0-9_-]{43}$/.test(text) && text !== ${JSON.stringify(clipboardBefore)} ? 'copied:' + text : '', () => '');
    })()`, stepLimit(8));
  if (!handed) await fail(`one press did not hand over an invite link, on the clipboard or in a card: ${JSON.stringify(await state(operator.tab))} — "${(await pageText(operator.tab)).slice(0, 300)}"`);
  report.copiedLink = String(handed).replace(/#.*/, '#…');
  if (!String(handed).startsWith('copied:')) await fail(`the link was not on the clipboard: ${handed}`);
  console.log(`ok: one press put an invite link on the operator's clipboard (${report.copiedLink})`);
  mark('one press handed the operator an invite link to give away');

  // The waiting card is the workspace's own action, offered to whoever it
  // lets invite: it is in the operator's search once the role reached them.
  // It is on the page before its list is, so it is read once it has answered.
  let card = null;
  for (const deadline = Date.now() + stepLimit(6); Date.now() < deadline && !card;) {
    await search(operator.tab, 'waiting', 'Waiting to join');
    card = await waitingCard(operator.tab);
    if (!card) await sleep(500);
  }
  if (!card) await fail(`the operator's search offered no wait list: ${(await pageText(operator.tab)).slice(0, 400)}`);
  report.waitingCard = card;
  if (!card.contacts.includes(address)) await fail(`the wait list shows ${JSON.stringify(card.contacts)} (${card.said ?? 'nothing said'}), not the address left there`);
  await dump(operator.tab, '3-waiting-card');
  console.log(`ok: the operator's wait list shows the address (${card.contacts.join(', ')})`);
  const invited = await pressText(operator.tab, 'Invite', '.tilo-waiting');
  if (!invited) await fail(`the wait list offers no Invite button: ${JSON.stringify(card.buttons)}`);
  report.cardAfterPress = await poll(operator.tab, `(() => document.querySelector('.tilo-waiting')?.outerHTML ?? '')()`, stepLimit(1));
  const mailed = await poll(operator.tab, `(() => {
    const sent = document.querySelector('.tilo-waiting-sent');
    const problem = document.querySelector('.tilo-waiting .tilo-entry-error');
    const share = document.querySelector('[data-card=share]');
    if (sent && !sent.hidden && sent.textContent.includes('emailed')) return 'mailed';
    if (problem && !problem.hidden && problem.textContent.trim()) return 'refused: ' + problem.textContent.trim();
    if (share) return 'shared instead: ' + share.textContent.trim().slice(0, 300);
    return '';
  })()`, stepLimit(10));
  if (mailed !== 'mailed') {
    report.inviteCard = await poll(operator.tab, `(() => document.querySelector('.tilo-waiting')?.outerHTML ?? '')()`, stepLimit(1)).catch(() => '');
    report.operatorConsole = (operator.tab.log ?? []).slice(-25);
    await fail(`the wait list did not mail the invitation (${mailed})`);
  }
  report.sent = await operator.tab.evaluate(`document.querySelector('.tilo-waiting-sent')?.textContent?.trim() ?? null`);
  const invitation = await mailArrived('the invitation mail', 2);
  report.invitationMail = {to: invitation.to, from: invitation.from, subject: invitation.subject};
  const inviteLink = mailLink(invitation, 'invite') ?? (invitation.text.match(/https?:\/\/\S+#(?:\/@[^\s/]+)?\/w\/[0-9a-f-]{36}\?invite=[A-Za-z0-9_-]{43}/) ?? [])[0];
  if (!inviteLink) await fail(`the invitation mail carries no link: ${(invitation.text ?? '').slice(0, 400)}`);
  report.inviteLink = inviteLink.replace(/#.*/, '#…');
  await pressText(operator.tab, 'Close', '.tilo-waiting');
  mark('the operator invited the address, and the invitation was mailed');
  console.log(`ok: the invitation mail reached ${invitation.to} (${invitation.subject})`);


  // -- 3. the stranger follows it, joins, and sets Tilo up through an agent ---
  step('the invitation bringing the stranger into a workspace of their own');
  await stranger.tab.navigate(inviteLink);
  // Their identity is founded on the way in: the 24 words are shown once,
  // before the workspace opens, to write down and keep.
  await keepRecoveryWords(stranger);
  // Someone let in off the wait list asked to get in, so the link joins them at
  // once, with no card to press: the invitation admits them to the Lounge, and
  // their page opens a workspace of their own to work in, with the Lounge one
  // switch away. A newcomer is not handed to the site's first screen either way;
  // the setup waits in their workspace, offered from their own card once they
  // used it.
  await until(stranger.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.workspace`,
    'the stranger joining through the invitation', stepLimit(8));
  await until(stranger.tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)
    && !!document.querySelector('#tilo-page [data-p]')`, 'the workspace the stranger joined', stepLimit(8));
  const overWorkspace = await stranger.tab.evaluate(`document.documentElement.dataset.firstScreen ?? null`);
  if (overWorkspace !== null) await fail(`the first screen came over the workspace the stranger joined (${overWorkspace})`);
  report.joined = JSON.parse(await until(stranger.tab, `(() => {
    const root = document.documentElement;
    const name = root.dataset.workspace ?? '';
    return root.dataset.arrival === 'done' && document.querySelector('#tilo-page [data-p]') && name
      ? JSON.stringify({workspace: name, title: document.title}) : '';
  })()`, 'the workspace the stranger joined, drawn', stepLimit(8)));
  await dump(stranger.tab, '4-joined-workspace');
  mark('the invitation brought the stranger into a workspace of their own, with no first screen over it');
  console.log(`ok: the invitation brought the stranger into ${report.joined.workspace}, which opened with no first screen over it (${report.joined.title})`);

  // Their own setup is offered inside the workspace: the person's card's
  // "Connect an agent or computer", on Devices and access, which shows the
  // first screen's flow with a fresh code. This person has an agent.
  step('the agent being set up in the workspace the person joined');
  await search(stranger.tab, 'devices and access', 'Devices and access');
  if (!await pressName(stranger.tab, 'Connect an agent or computer', stepLimit(8))) {
    await fail(`Devices and access offers no "Connect an agent or computer" to press: ${(await pageText(stranger.tab)).slice(0, 1200)}`);
  }
  // A press on a window just opened can be the one that makes it the window
  // in front: a person presses the button again.
  let setupShown = await poll(stranger.tab, firstScreenShown, stepLimit(4));
  if (!setupShown) {
    await pressName(stranger.tab, 'Connect an agent or computer', stepLimit(4));
    setupShown = await poll(stranger.tab, firstScreenShown, stepLimit(6));
  }
  if (!setupShown) {
    await dump(stranger.tab, '4-connect-agent');
    await fail(`the setup opened from Devices and access never showed: ${JSON.stringify(await state(stranger.tab))} — ${(await pageText(stranger.tab)).slice(0, 1200)}`);
  }
  report.firstScreenShown = await stranger.tab.evaluate(firstScreenShown);
  if (!report.firstScreenShown) await fail('the first screen is in the page but not shown');
  report.firstScreen = await stranger.tab.evaluate(`({
    heading: document.getElementById('first-screen-heading')?.textContent?.trim() ?? null,
    prompt: document.getElementById('first-screen-offer')?.textContent?.trim() ?? null,
    skip: document.getElementById('first-screen-skip')?.textContent?.trim() ?? null,
    manual: document.getElementById('first-screen-manual-toggle')?.textContent?.trim() ?? null,
  })`);
  if (!report.firstScreen.skip) await fail('the first screen offers no way past it for a person without an agent');
  await until(stranger.tab, `document.getElementById('first-screen')?.dataset.setup === 'ready'`, 'the setup prompt made', stepLimit(6));
  const beforeCopy = await stranger.tab.evaluate(`navigator.clipboard.readText().catch(() => '')`).catch(() => '');
  await stranger.tab.send('Browser.grantPermissions', {origin: siteOrigin, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  await pressId(stranger.tab, 'first-screen-agent');
  await until(stranger.tab, `document.documentElement.dataset.firstScreen === 'waiting'`, 'the first screen answering the click', stepLimit(4));
  const prompt = await copied(stranger.tab, beforeCopy);
  report.prompt = prompt;
  const setup = commandOf(prompt);
  if (!setup) await fail(`the prompt names no tilo login ticket: ${prompt}`);
  const env = await agentEnv();
  const login = await tilo(env, setup.ticket ? ['login', setup.ticket] : ['login', setup.code, ...(setup.site ? ['--site', setup.site] : [])]);
  report.login = login;
  if (login.code !== 0) await fail(`tilo login failed (${login.code}): ${login.stderr}${login.stdout}`);
  // The agent adds Tilo's server to its own configuration, as the login's
  // words ask, and confirms it: the setup leaves only once it has.
  writeFileSync(agentConfiguration, `${JSON.stringify({mcpServers: {tilo: {type: 'stdio', command: tiloBin, args: ['mcp'], env: {}}}})}\n`);
  const checked = await tilo(env, ['agent', 'check', '--config', agentConfiguration]);
  report.check = checked;
  if (checked.code !== 0) await fail(`tilo agent check did not confirm Tilo's server (${checked.code}): ${checked.stderr}${checked.stdout}`);
  console.log('ok: tilo login ran and the agent confirmed Tilo’s MCP server in its configuration');
  await until(stranger.tab, `!${firstScreenShown}`, 'the first screen leaving', stepLimit(4));
  const still = JSON.parse(await until(stranger.tab, `(() => {
    const root = document.documentElement;
    const name = root.dataset.workspace ?? '';
    return root.dataset.arrival === 'done' && document.querySelector('#tilo-page [data-p]') && name
      ? JSON.stringify({workspace: name, title: document.title}) : '';
  })()`, 'the workspace drawn again once the setup left', stepLimit(3)));
  if (still.workspace !== report.joined.workspace) {
    await fail(`the setup moved the person out of the workspace they joined: ${still.workspace}, not ${report.joined.workspace}`);
  }
  report.landed = await person(stranger.tab);
  report.workspace = still.title;
  await dump(stranger.tab, '5-joined-workspace-set-up');
  mark('the agent was set up in the workspace the person joined');
  console.log(`ok: the person's agent is in the workspace they joined (${still.title})`);

  // Let in, not only named: no failure screen over the workspace. Then a
  // second tab of the same browser, which holds a device key of its own (one
  // per tab), opens the workspace the person is in as theirs, and the first
  // tab stays in: the agent's entry never takes the person's own tabs out.
  step('a second tab of the person entering the workspace they are in');
  const failureOf = tab => tab.evaluate(`document.documentElement.dataset.failure
    ? document.documentElement.dataset.failure + ': ' + (document.body.innerText || '').replace(/\\s+/g, ' ').slice(0, 300) : ''`).catch(() => '');
  const refusedFirst = await failureOf(stranger.tab);
  if (refusedFirst) await fail(`the workspace refused the tab after the agent entered it: ${refusedFirst}`);
  const second = await stranger.browser.tab();
  second.log ??= [];
  said.push({name: 'stranger-second-tab', tab: second});
  await second.send('Emulation.setDeviceMetricsOverride', shellMetrics).catch(() => {});
  await second.navigate(site);
  await until(second, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`,
    'a second tab of the person arriving', stepLimit(8));
  const refusedSecond = await failureOf(second);
  if (refusedSecond) await fail(`a second tab of the person was not let into the workspace they are in: ${refusedSecond}`);
  await until(second, `!!(${JSON.stringify(report.workspace)} === document.title)`,
    'the second tab in the workspace the person is in', stepLimit(4));
  await dump(second, '5-joined-workspace-second-tab');
  const refusedAfter = await failureOf(stranger.tab);
  if (refusedAfter) await fail(`the first tab lost the workspace when the second came: ${refusedAfter}`);
  await second.close();
  mark('a second tab of the person entered the workspace they are in, and the first stayed in');
  console.log('ok: a second tab of the person enters the workspace they are in, and the first stays in');

  // -- 3b. the same person on a browser new to the site signs in with their agent
  // The invite-only entry turns this browser away to its wait list; "Sign in
  // with your agent" shows a sign-in prompt with its code, and the computer
  // `tilo login` set up above claims it. That computer is a device of the
  // person (their root stays in the tab that set it up), and a device of the
  // person signs a browser tab of theirs in.
  step('a browser new to the site signing the person in with their agent');
  const returning = await visit('returning');
  await arrive(returning, {expected: 'refused'});
  await until(returning.tab, `document.documentElement.dataset.refusal === 'wait-list'`, 'the wait list for a browser new to the site', stepLimit(6));
  await pressId(returning.tab, 'arrival-sign-in-agent');
  // The prompt a person copies, read from what the screen shows: the code it
  // names, once the screen is ready to be copied.
  const signInShown = `(() => {
    const text = (document.body?.innerText || '').replace(/\\s+/g, ' ');
    const named = text.match(/(?:tilo|sh -s) login [2-9A-Z]{3}-[2-9A-Z]{4}(?: --site \\S+?)?(?:,|\\s|$)/);
    return named ? named[0] : '';
  })()`;
  const signInPrompt = await poll(returning.tab, signInShown, stepLimit(6));
  if (!signInPrompt) {
    await dump(returning.tab, '5-sign-in-with-agent');
    await fail(`"Sign in with your agent" showed no sign-in prompt: ${JSON.stringify(await state(returning.tab))} — ${await pageText(returning.tab)}`);
  }
  report.signInPrompt = signInPrompt;
  await dump(returning.tab, '5-sign-in-with-agent');
  const signIn = commandOf(signInPrompt);
  if (!signIn) await fail(`the sign-in prompt names no ticket or code: ${signInPrompt}`);
  const claimed = await tilo(env, signIn.kind === 'device add'
    ? ['device', 'add', signIn.ticket]
    : ['login', signIn.code, ...(signIn.site ? ['--site', signIn.site] : [])]);
  report.signInLogin = claimed;
  if (claimed.code !== 0) await fail(`tilo login of the sign-in code failed (${claimed.code}): ${claimed.stderr}${claimed.stdout}`);
  // The person's root stays in the tab that set it up, so the computer that ran `tilo device add`
  // holds none: it hands the signing on to the person's devices, and the first tab, which does
  // hold their root, asks about the browser it would sign in — naming the device and what
  // certifying it makes it — and approves it.
  if (signIn.kind === 'device add') {
    if (!/handed the signing on/.test(claimed.stdout)) {
      await fail(`tilo device add did not hand the signing on from a computer with no root: ${claimed.stdout}${claimed.stderr}`);
    }
    const askedAbout = await until(stranger.tab, `(() => {
      const bar = document.getElementById('tilo-sign-in');
      return bar && !bar.hidden ? bar.innerText.trim() : '';
    })()`, 'the first tab asking about the browser signing in', stepLimit(8));
    report.signInAsked = askedAbout;
    if (!/becomes one of your devices/.test(askedAbout)) {
      await fail(`the first tab's ask names no device of the person's: ${askedAbout}`);
    }
    await pressId(stranger.tab, 'tilo-sign-in-allow');
    mark('the first tab approved the browser signing in');
  }
  // The person asked their agent to sign them in on this tab, so the tab asks
  // them nothing about whose device it becomes: that card, which warns of a
  // link someone else made, is for a link handed to a device.
  const signedInOrAsked = await until(returning.tab, `(() => {
    if (document.querySelector('[data-card="pair-whose"]')) return 'asked';
    const name = document.documentElement.dataset.workspace ?? '';
    const drawn = document.documentElement.dataset.arrival === 'done'
      && !!document.querySelector('#tilo-page [data-p]');
    return drawn ? name : '';
  })()`, 'the signed-in tab in the workspace the person is in', stepLimit(8));
  if (signedInOrAsked === 'asked') {
    await fail('the tab its person signed in with their agent asked whose device it becomes, as for a link someone else made');
  }
  const signedIn = await person(returning.tab);
  report.signedIn = signedIn;
  if (!report.landed?.user || signedIn.user !== report.landed.user) {
    await fail(`the agent signed in ${signedIn.user ?? 'nobody'}, not ${report.landed?.user ?? 'the person who set up'}`);
  }
  await dump(returning.tab, '5-signed-in-with-agent');
  await returning.browser.close().catch(() => {});
  mark('a browser new to the site signed the person in with their agent');
  console.log(`ok: "Sign in with your agent" showed its code, and the person's computer signed the tab in to ${signedIn.title}`);

  // -- 4. a second workspace -------------------------------------------------
  step('a second workspace the person makes');
  await search(stranger.tab, 'new workspace', 'New workspace');
  await until(stranger.tab, `document.querySelector('.tilo-entry-card input') !== null`, 'the card asking what to name it', stepLimit(6));
  const named = 'Studio';
  await stranger.tab.evaluate(`(() => {
    const input = document.querySelector('.tilo-entry-card input');
    input.value = ${JSON.stringify(named)};
    input.dispatchEvent(new Event('input', {bubbles: true}));
  })()`);
  // The workspace the page opens, and the one this browser remembers by that
  // name: the tab's title names the space in front on the workspace's
  // desktop, not the workspace, so it is not what says where the page is.
  const opened = `(() => {
    const chosen = JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null');
    const entered = JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]');
    const kept = (Array.isArray(entered) ? entered : []).find(entry => entry.workspace === chosen?.workspace);
    // The entry says it opened the workspace; the page the server renders
    // for it (\`/w/<workspace>\`) is past the entry and only says it arrived.
    const open = (document.documentElement.dataset.entry === 'open' || location.pathname.startsWith('/w/'))
      && ['done', 'stopped'].includes(document.documentElement.dataset.arrival);
    return open && kept?.name === ${JSON.stringify(named)} ? chosen.workspace : '';
  })()`;
  const before = await stranger.tab.evaluate(opened).catch(() => '');
  if (before) await fail(`a workspace named ${named} was open before the person made one: ${before}`);
  await pressText(stranger.tab, 'Create', '.tilo-entry-card');
  const made = await until(stranger.tab, opened, 'the second workspace opening', stepLimit(10));
  if (made === report.landed?.workspace) await fail(`the second workspace is the first one again: ${made}`);
  report.secondWorkspace = {workspace: made, name: named, title: await stranger.tab.evaluate(`document.title`)};
  await dump(stranger.tab, '7-second-workspace');
  console.log(`ok: the person made and opened a second workspace (${named}, ${made})`);
  mark('the person made a second workspace');

  // -- 5. a visitor with no invitation is still out --------------------------
  step('a visitor with no invitation, who still cannot get in');
  const visitor = await visit('visitor');
  await arrive(visitor, {expected: 'refused'});
  await until(visitor.tab, `document.documentElement.dataset.refusal === 'wait-list'`, 'the last visitor meeting the wait list', stepLimit(6));
  const other = 'grace@example.org';
  await visitor.tab.evaluate(`(() => {
    const field = document.getElementById('arrival-wait-email');
    field.value = ${JSON.stringify(other)};
    field.dispatchEvent(new Event('input', {bubbles: true}));
    document.querySelector('#arrival-wait button').click();
  })()`);
  await until(visitor.tab, `document.documentElement.dataset.waitList === 'inbox'`, 'the last visitor leaving an address', stepLimit(6));
  const memberships = await visitor.tab.evaluate(`globalThis.localStorage?.getItem('tilo-host-memberships') ?? null`);
  if (memberships) await fail(`a visitor with no invitation was let in: ${memberships}`);
  const third = await mailArrived('the last visitor\'s confirmation mail', 3);
  report.lastMail = {to: third.to, subject: third.subject};
  await dump(visitor.tab, '8-without-an-invitation');
  console.log(`ok: a visitor with no invitation is still on the wait list (${third.to})`);
  mark('a visitor with no invitation left the wait list and got no further');
  await finished();
} catch (error) {
  await fail(error?.stack ? `${error.message}\n${error.stack}` : String(error));
}
