// A stranger's way into tilo.sh, in real browsers, from the first visit to a
// workspace of their own: what the site says to someone it does not know yet,
// what the mail they get opens, what the person who runs the host does about
// it, and where the person ends up once they are in.
//
// The story this drives, one step per mark below:
//
//   0. the person who runs the host arrives, and the operator declares them an
//      owner through the workspace's operator socket — how a fresh
//      invitation-only Lounge gets its first member; the door lets them in,
//      and the workspace makes them an owner once they are;
//   1. a stranger arrives: the door's wait list, not the setup screen, and an
//      address left on it is confirmed through the mail the door sends;
//   2. the operator hands an invite link over in one press, and invites that
//      address from the workspace's own wait list, which mails the invitation;
//   3. the stranger follows the invitation's link and joins; a person not set
//      up here is handed to the site, whose first screen sets Tilo up through
//      their agent (`tilo login`, which adds Tilo's MCP server) and opens a
//      workspace of their own, not the shared desktop they landed in;
//   4. they keep themselves with a passkey, their browser forgets the site,
//      and the passkey brings them back to that same workspace;
//   5. they make a second workspace and are in it;
//   6. someone else with no invitation still cannot get in, and leaves an
//      address on the wait list.
//
// It stops at step 4: the passkey signs the person in on a browser that forgot
// the site, and the host then refuses to certify that tab as one of their
// devices ("only a member approves devices here"), so the walk ends there.
// Steps 0 to 3 are what it proves today.
//
// The mails are the door's own, kept by the mail sink the stack points it at:
// the run reads each one's link out of the file it landed in, so what the flow
// proves is the mail a person would get.
//
// usage: node onboarding.mjs SITE RELAY WORK TILO_BIN OWNER_SOCKET
//
// SITE is the web entry's origin (the site people visit), RELAY the workspace
// host's, WORK the run's directory (the stack's state, the mail sink's files
// and every page's screenshots and console live there), TILO_BIN the `tilo`
// command an agent runs, and OWNER_SOCKET the running Lounge's operator socket:
// the driver declares the operator an owner through it, as the deployment
// does. The stack is plain http on this machine, so no certificate stands
// between the browsers, the agent's command and it.
import {execFile, execFileSync, spawn} from 'node:child_process';
import {chmodSync, mkdirSync, readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {FIRST_RUN_SEEN, launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {stepLimit, deskMetrics, machine} from './waits.mjs';

const [site, relay, work, tiloBin, ownerSocket] = process.argv.slice(2);
if (!site || !relay || !work || !tiloBin || !ownerSocket) {
  console.error('usage: node onboarding.mjs SITE RELAY WORK TILO_BIN OWNER_SOCKET');
  process.exit(2);
}
// The mail sink the stack points the door at (`stack.sh mail` reads the same).
const mailDir = join(work, 'state', 'mail');
const computer = join(work, 'computer');
mkdirSync(mailDir, {recursive: true});
const mark = clock();
const report = {site, relay, at: new Date().toISOString(), steps: []};
const browsers = [];
const said = [];

/** Where the host is as the pages know it: a workspace is stored per origin. */
const hostOrigin = new URL(relay).origin;
/** The site's own origin: what its storage belongs to. */
const siteOrigin = new URL(site).origin;

const browserArgs = [
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];

const pageText = tab => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 1200)`)
  .catch(() => '');
const state = tab => tab.evaluate(`JSON.stringify({
  arrival: document.documentElement.dataset.arrival ?? null,
  door: document.documentElement.dataset.door ?? null,
  waitList: document.documentElement.dataset.waitList ?? null,
  entry: document.documentElement.dataset.entry ?? null,
  firstRun: document.documentElement.dataset.firstRun ?? null,
  title: document.title,
})`).then(JSON.parse).catch(() => ({}));

const dump = async (tab, name) => {
  const png = await Promise.race([tab.screenshot().catch(() => null), sleep(stepLimit(2)).then(() => null)]);
  if (png) writeFileSync(join(work, `${name}.png`), png);
  writeFileSync(join(work, `${name}.log`), (tab.log ?? []).join('\n'));
};

const fail = async message => {
  if (!report.error) report.error = message;
  console.error(`FAIL: ${message}`);
  console.error(`the machine as it failed: ${machine().text}`);
  for (const {name, tab} of said) {
    const lines = (tab.log ?? []).slice(-40);
    if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
  }
  for (const {name, tab} of said) {
    await dump(tab, `failed-${name}`).catch(() => {});
    report[`${name}Text`] = await pageText(tab);
    report[`${name}State`] = await state(tab);
  }
  writeFileSync(join(work, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  process.exit(1);
};
const finished = async () => {
  for (const {name, tab} of said) await dump(tab, name).catch(() => {});
  writeFileSync(join(work, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  console.log(JSON.stringify(report, null, 2));
  process.exit(0);
};

const failures = tab => (tab.log ?? []).filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));
const checkPage = async (tab, name) => {
  const found = failures(tab);
  if (found.length) {
    report[`${name}State`] = await state(tab);
    await fail(`the page failed while ${name}: ${found.slice(0, 5).join(' | ')}`);
  }
};
/** Wait until the page answers `expression` true, or say what it was doing. */
/** Wait until the page answers `expression` something other than an empty
 * string, or say '' when it never did: for reading what a card says next, or
 * what went wrong, without the wait failing the run. */
const poll = async (tab, expression, timeout = stepLimit(4)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => '');
    if (value) return value;
    if (Date.now() > deadline) return '';
    await sleep(200);
  }
};
/** Wait until the page answers `expression` true, or say what it was doing. */
const until = async (tab, expression, what, timeout = stepLimit(4)) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await fail(`${what} never happened: ${JSON.stringify(await state(tab))} — "${(await pageText(tab)).slice(0, 400)}"`);
    await sleep(200);
  }
};
/** A page of the person's own: a browser of its own, and a tab that has not
 * been to the site before, so the site treats it as a first visit. A person
 * who has been here before (`setupDone`) opens the site itself, not its first
 * screen: the workspace's own desktop, as the people who run it see it. */
const visit = async (name, {passkey = false, setupDone = false} = {}) => {
  const browser = await launch({args: browserArgs});
  browsers.push(browser);
  const tab = await browser.tab({firstRun: true});
  tab.log ??= [];
  if (setupDone) await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FIRST_RUN_SEEN});
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Log.enable', {}).catch(() => {});
  const asked = new Map();
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
    else if (method === 'Network.responseReceived' && params.response.status >= 400) tab.log.push(`HTTP ${params.response.status} ${params.response.url}`);
    else if (method === 'Log.entryAdded') tab.log.push(`LOG ${params.entry.level} ${params.entry.text}`);
    else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 500));
    }
  });
  await tab.send('Emulation.setDeviceMetricsOverride', deskMetrics).catch(() => {});
  if (passkey) await authenticator(tab);
  said.push({name, tab});
  return {name, tab, browser};
};
/** The platform authenticator a person's device has: resident, verified, and
 * answering PRF, which is what opens a Tilo identity. */
const authenticator = async tab => {
  const enabled = await tab.send('WebAuthn.enable', {enableUI: false});
  if (enabled.error) await fail(`no WebAuthn: ${enabled.error.message}`);
  const added = await tab.send('WebAuthn.addVirtualAuthenticator', {options: {protocol: 'ctap2', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, isUserVerified: true, hasPrf: true}});
  if (added.error) await fail(`no virtual authenticator: ${added.error.message}`);
};
/** The page's own identity module: the one it holds its person in. */
const identityModule = `await import(new URL('identity.mjs', new URL('.', (document.querySelector('link[rel="modulepreload"][href], script[type="module"][src]').href
  || document.querySelector('script[type="module"][src]').src))).href)`;
/** Who this page is, and which workspace it holds. */
const person = tab => tab.evaluate(`JSON.stringify({
  user: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.userPublicKey ?? null; } catch { return null; } })(),
  room: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null')?.room ?? null; } catch { return null; } })(),
  title: document.title,
})`).then(JSON.parse).catch(() => ({}));
/** The person's own workspace, as the page titles it: never the Lounge's. */
const ownWorkspace = tab => tab.evaluate(`/^Tilo · /.test(document.title) && !/lounge/i.test(document.title) && document.title`).catch(() => '');

// -- the desktop -------------------------------------------------------------
const canvasBox = tab => tab.evaluate(`(() => {
  const rect = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return JSON.stringify(rect ? {x: rect.left, y: rect.top} : null);
})()`);
const clickAt = async (tab, x, y) => {
  await Promise.all([
    tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}),
    ...['mousePressed', 'mouseReleased'].map(type => tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1})),
  ]);
  await sleep(400);
};
const key = async (tab, name, modifiers = 0) => {
  const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38};
  const pressed = {key: name, code: name.length === 1 ? `Key${name.toUpperCase()}` : name,
    windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await Promise.all([
    tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...pressed, ...(name === 'Enter' ? {text: '\r'} : {})}),
    tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...pressed}),
  ]);
};
const type = async (tab, text) => {
  await Promise.all([...text].flatMap(ch => [
    tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch}),
    tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch}),
  ]));
  await sleep(600);
};
/** The rows the command palette offers once it has answered `query`. */
const searchRows = async (tab, query) => {
  const asked = JSON.stringify(query);
  const answered = `(() => {
    const s = globalThis.tiloMount?.state?.() ?? null;
    const card = s?.searchCard ?? null;
    return card && s.searchSearching === false && card.query === ${asked} ? JSON.stringify(card.rows ?? []) : '';
  })()`;
  return JSON.parse(await until(tab, answered, `the desk answering "${query}"`, stepLimit(4)));
};
/** A person's search: the chord, the query, and Enter on the row titled
 * `title` — an app, or one of the workspace's own actions. */
const search = async (tab, query, title) => {
  if (!await tab.evaluate('!!globalThis.tiloMount?.state()?.searchOpen')) {
    const box = JSON.parse(await canvasBox(tab));
    await clickAt(tab, box.x + deskMetrics.width / 2, box.y + deskMetrics.height - 100);
    await key(tab, 'k', 2);
  }
  await until(tab, '!!globalThis.tiloMount?.state()?.searchOpen', 'the search opening', stepLimit(3));
  await type(tab, query);
  let rows = [];
  const deadline = Date.now() + stepLimit(4);
  for (;;) {
    rows = await searchRows(tab, query).catch(() => []);
    if (rows[0]?.title === title) break;
    if (Date.now() > deadline) {
      await fail(`the palette offers no "${title}" first for "${query}": ${JSON.stringify(rows.map(row => row.title))}`);
    }
    await sleep(300);
  }
  await key(tab, 'Enter');
  await until(tab, '!globalThis.tiloMount?.state()?.searchOpen', `the search closing after "${title}"`, stepLimit(4));
  return rows;
};
/** A real pointer press on the desktop's node named `name`, once it is drawn. */
const pressName = async (tab, name) => {
  for (const deadline = Date.now() + stepLimit(3); Date.now() < deadline;) {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    const box = JSON.parse(await tab.evaluate(`(() => {
      const found = [];
      const node = n => { found.push({name: n.name ?? '', bounds: n.bounds ?? n.rect ?? null}); (n.children || []).forEach(node); };
      const walk = n => { if (n.semantics) node(n.semantics); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      const hit = found.find(n => n.name === ${JSON.stringify(name)} && n.bounds);
      if (!hit) return '';
      const b = Array.isArray(hit.bounds) ? hit.bounds : [hit.bounds.x0 ?? hit.bounds.x, hit.bounds.y0 ?? hit.bounds.y,
        hit.bounds.x1 ?? hit.bounds.x + hit.bounds.width, hit.bounds.y1 ?? hit.bounds.y + hit.bounds.height];
      const corner = globalThis.tiloMount.canvas.getBoundingClientRect();
      return JSON.stringify({x: Math.round(corner.left + (b[0] + b[2]) / 2), y: Math.round(corner.top + (b[1] + b[3]) / 2)});
    })()`).catch(() => ''));
    if (box) { await clickAt(tab, box.x, box.y); return true; }
    await sleep(300);
  }
  return false;
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
/** A click on the DOM button whose text is `text`, in the card `within`. */
const pressText = (tab, text, within = 'body') => tab.evaluate(`(() => {
  const button = [...document.querySelectorAll(${JSON.stringify(within)} + ' button')]
    .find(candidate => candidate.textContent.trim() === ${JSON.stringify(text)} && !candidate.disabled);
  if (!button) return false;
  button.click();
  return true;
})()`);

// -- the mails ---------------------------------------------------------------
/** The mails the door sent, oldest first: what the sink kept of each. */
const mails = () => readdirSync(mailDir).filter(name => name.endsWith('.json')).sort()
  .map(name => JSON.parse(readFileSync(join(mailDir, name), 'utf8')));
const waitForMail = async (what, count) => {
  const deadline = Date.now() + stepLimit(6);
  for (;;) {
    const kept = mails();
    if (kept.length >= count) return kept[count - 1];
    if (Date.now() > deadline) await fail(`${what} never reached the mail sink (${kept.length} mails: ${JSON.stringify(kept.map(mail => mail.subject))})`);
    await sleep(250);
  }
};
/** The link a mail carries whose path ends in `marker`'s variable, e.g. the
 * confirmation's `wait-list-confirm=<secret>`. */
const mailLink = (mail, marker) => {
  const text = `${mail.text ?? ''}\n${mail.html ?? ''}`;
  return text.match(new RegExp(`https?://[^\\s"'<>]*${marker}[=:][0-9a-zA-Z-]+`))?.[0] ?? null;
};

/** Whether the first screen is in front of the person. Its element is in every
 * page the entry serves (`#first-run`), so what shows is whether the page set
 * `data-first-run` and displayed it. */
const firstScreenShown = `(document.documentElement.dataset.firstRun !== undefined
  && getComputedStyle(document.getElementById('first-run')).display !== 'none')`;

// -- tilo.sh as a person meets it --------------------------------------------
/** A person arriving at the site, in a tab that has not been here before.
 * `expected: 'let in'` tries again while the door refuses: the door that let
 * the declaration in holds it, so a person declared an owner just now may meet
 * the closed door once more before it opens for them, and comes back — as the
 * operator does — instead of landing on the wait list. */
const arrive = async (page, {expected}) => {
  const deadline = Date.now() + stepLimit(6);
  for (;;) {
    await page.tab.navigate(site);
    await until(page.tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, `${page.name} arriving`, stepLimit(6));
    const seen = await state(page.tab);
    report.steps.push({step: `${page.name} arrived`, ...seen});
    if (seen.arrival === 'done' || expected === 'refused') return seen;
    if (Date.now() > deadline) await fail(`${page.name} did not arrive: ${JSON.stringify(seen)} — "${(await pageText(page.tab)).slice(0, 300)}"`);
    await sleep(500);
  }
};

// -- the deployment's declaration of its people -------------------------------
/** Declare `user` (hex) an owner of the Lounge, as the deployment does: the
 * operator asks the running workspace over its operator socket, and the
 * workspace's door lets them in and its governance makes them an owner the
 * moment they are a member. */
const declareOwner = async user => {
  execFileSync(
    'python3',
    ['-I', fileURLToPath(new URL('../declare-owner', import.meta.url)), ownerSocket, user],
    {stdio: ['ignore', 'inherit', 'inherit']},
  );
};

// The agent's computer: its own home, a `claude` that records what the command
// asked it and writes the server where a real one would, and the test
// certificate the command trusts for this run's site.
mkdirSync(join(computer, 'bin'), {recursive: true});
const claudeCalls = join(computer, 'claude-calls.txt');
writeFileSync(join(computer, 'bin', 'claude'), `#!/bin/sh
printf '%s\\n' "$*" >> '${claudeCalls}'
[ "$1 $2" = "mcp add" ] || exit 0
shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --scope|-s) shift 2 ;;
    --*) shift ;;
    *) break ;;
  esac
done
name=$1; shift
[ "$1" = "--" ] && shift
command=$1; shift
args=
for arg in "$@"; do args="$args\\"$arg\\","; done
printf '{"mcpServers":{"%s":{"type":"stdio","command":"%s","args":[%s],"env":{}}}}\\n' \\
  "$name" "$command" "\${args%,}" > "$HOME/.claude.json"
`);
chmodSync(join(computer, 'bin', 'claude'), 0o755);
const agentEnv = async () => ({
  PATH: `${join(computer, 'bin')}:${process.env.PATH}`,
  HOME: computer,
  XDG_STATE_HOME: join(computer, 'state'),
  XDG_CONFIG_HOME: join(computer, 'config'),
  CLAUDECODE: '1',
});
const tilo = async (env, args) => new Promise(resolve => {
  execFile(tiloBin, args, {env, timeout: 180_000, maxBuffer: 1 << 20},
    (error, stdout, stderr) => resolve({code: error ? (error.code ?? 1) : 0, stdout, stderr}));
});
/** What the first screen copies, read from the clipboard: a person's click
 * writes it inside the gesture, and the browser lands the write a moment
 * later, so a prompt an earlier copy left there is not this one. */
const copied = async (tab, before) => until(tab,
  `navigator.clipboard.readText().then(text => text.match(/(?:tilo|sh -s) login /) && text !== ${JSON.stringify(before)} ? text : '', () => '')`,
  'the prompt on the clipboard', stepLimit(4));
const commandOf = prompt => {
  const match = prompt.match(/(?:tilo|sh -s) login ([2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})(?: --site (\S+?))?(?:,|\s|$)/);
  return match ? {code: match[1], site: match[2] ?? null} : null;
};

try {
  // -- 0. the operator, declared by the deployment, is let in --------------
  // A fresh invitation-only Lounge has nobody in it: the people the operator
  // declares are the ones its door lets in, so the person who runs it arrives
  // first, and this run declares their key through the operator socket the way
  // `stack.sh owner` does.
  const operator = await visit('operator', {setupDone: true});
  await arrive(operator, {expected: 'refused'});
  const operatorPerson = await operator.tab.evaluate(`(async () => {
    const id = ${identityModule};
    return (await id.browserIdentity()).userPublicKey;
  })()`).catch(() => null);
  if (!operatorPerson) await fail('the operator\'s tab made no person to declare');
  await declareOwner(operatorPerson);
  report.operator = {user: operatorPerson};
  await arrive(operator, {expected: 'let in'});
  await until(operator.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.room`,
    'the door letting in the person the deployment declared', stepLimit(8));
  mark('the operator the deployment declares is let in by the closed door');

  // -- 1. a stranger at the wait list ----------------------------------------
  const stranger = await visit('stranger', {passkey: true});
  await arrive(stranger, {expected: 'refused'});
  await until(stranger.tab, `document.documentElement.dataset.door === 'wait-list'`, "the door showing its wait list", stepLimit(6));
  await until(stranger.tab, `document.documentElement.dataset.waitList === 'form'`, 'the wait list offering its field');
  await until(stranger.tab, `!${firstScreenShown}`, 'the setup screen leaving the door');
  report.waitList = await stranger.tab.evaluate(`({
    heading: document.getElementById('arrival-door-heading')?.textContent?.trim() ?? null,
    body: document.getElementById('arrival-door-body')?.textContent?.trim() ?? null,
    note: document.getElementById('arrival-wait-note')?.textContent?.trim() ?? null,
    button: document.querySelector('#arrival-wait button')?.textContent?.trim() ?? null,
    signIn: document.getElementById('arrival-sign-in-button')?.textContent?.trim() ?? null,
    contact: document.getElementById('arrival-wait-contact')?.textContent?.trim() ?? null,
  })`);
  await dump(stranger.tab, '1-wait-list');
  console.log(`ok: a stranger met the wait list, not the setup screen (${report.waitList.heading})`);
  mark('a stranger met the wait list');

  const address = 'ada@example.org';
  await stranger.tab.evaluate(`(() => {
    const field = document.getElementById('arrival-wait-email');
    field.value = ${JSON.stringify(address)};
    field.dispatchEvent(new Event('input', {bubbles: true}));
    document.querySelector('#arrival-wait button').click();
  })()`);
  await until(stranger.tab, `document.documentElement.dataset.waitList === 'inbox'`, 'the address being left', stepLimit(4));
  report.inbox = await stranger.tab.evaluate(`document.getElementById('arrival-wait-done')?.textContent?.trim() ?? null`);
  const confirmation = await waitForMail('the confirmation mail', 1);
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
  await operator.tab.send('Browser.grantPermissions', {origin: siteOrigin,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  // Only the tab in front may read the clipboard of a browser here: the
  // operator's is one of several.
  await operator.tab.send('Page.bringToFront', {}).catch(() => {});
  await operator.tab.send('Emulation.setFocusEmulationEnabled', {enabled: true}).catch(() => {});
  report.clipboardFocused = await operator.tab.evaluate('document.hasFocus()').catch(() => null);
  const clipboardBefore = await operator.tab.evaluate(`navigator.clipboard.readText().catch(() => '')`).catch(() => '');
  await search(operator.tab, 'copy invite', 'Copy an invite link');
  const handed = await poll(operator.tab,
    `(() => {
      const shared = document.querySelector('[data-card=share] input[name=link]')?.value;
      if (shared && shared !== ${JSON.stringify(clipboardBefore)}) return 'shared:' + shared;
      return navigator.clipboard.readText().then(text => text.includes('/i/') && text !== ${JSON.stringify(clipboardBefore)} ? 'copied:' + text : '', () => '');
    })()`, stepLimit(8));
  if (!handed) await fail(`one press did not hand over an invite link, on the clipboard or in a card: ${JSON.stringify(await state(operator.tab))} — "${(await pageText(operator.tab)).slice(0, 300)}"`);
  report.copiedLink = String(handed).replace(/#.*/, '#…');
  if (!String(handed).startsWith('copied:')) await fail(`the link was not on the clipboard: ${handed}`);
  console.log(`ok: one press put an invite link on the operator's clipboard (${report.copiedLink})`);
  mark('one press handed the operator an invite link to give away');

  const cardSeen = async () => {
    for (const deadline = Date.now() + stepLimit(6); Date.now() < deadline;) {
      // The card is on the page before its list is: read it once it has
      // answered with rows, with nobody on it, or with what went wrong.
      const shown = await operator.tab.evaluate(`(() => {
        const card = document.querySelector('.tilo-waiting');
        if (!card) return '';
        const error = card.querySelector('.tilo-entry-error');
        const rows = [...card.querySelectorAll('.tilo-waiting-contact')].map(node => node.textContent.trim());
        const empty = card.querySelector('.tilo-waiting-none');
        if (!rows.length && !empty && !(error && !error.hidden)) return '';
        return JSON.stringify({contacts: rows,
          said: error && !error.hidden ? error.textContent.trim() : null,
          buttons: [...card.querySelectorAll('li button')].map(node => node.textContent.trim())});
      })()`).catch(() => '');
      if (shown) return JSON.parse(shown);
      await sleep(300);
    }
    return null;
  };
  // The waiting card is the workspace's own action, offered to whoever it
  // lets invite: it is in the operator's search once the role reached them.
  let card = null;
  for (const deadline = Date.now() + stepLimit(6); Date.now() < deadline && !card;) {
    await search(operator.tab, 'waiting', 'Waiting to join');
    card = await cardSeen();
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
  const invitation = await waitForMail('the invitation mail', 2);
  report.invitationMail = {to: invitation.to, from: invitation.from, subject: invitation.subject};
  const inviteLink = mailLink(invitation, 'invite') ?? (invitation.text.match(/https?:\/\/\S+\/i\/[A-Za-z0-9_-]{16}#[A-Za-z0-9_-]{43}/) ?? [])[0];
  if (!inviteLink) await fail(`the invitation mail carries no link: ${(invitation.text ?? '').slice(0, 400)}`);
  report.inviteLink = inviteLink.replace(/#.*/, '#…');
  await pressText(operator.tab, 'Close', '.tilo-waiting');
  mark('the operator invited the address, and the invitation was mailed');
  console.log(`ok: the invitation mail reached ${invitation.to} (${invitation.subject})`);


  // -- 3. the stranger follows it, joins, and sets Tilo up through an agent ---
  await stranger.tab.navigate(inviteLink);
  const invitedCard = await until(stranger.tab, `(() => {
    const text = (document.body.innerText || '').replace(/\\s+/g, ' ').trim();
    return text.includes('invites you to') ? text : '';
  })()`, 'the invitation naming who invited them', stepLimit(6));
  report.invitationCard = String(invitedCard).slice(0, 400);
  await dump(stranger.tab, '4-invitation');
  if (!await pressText(stranger.tab, 'Join')) await fail(`the invitation offered no Join: ${report.invitationCard}`);
  // Joined: the page enters a workspace other than the one the Lounge listed
  // for it before, and the first screen of a first visit is over it.
  await until(stranger.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.room`,
    'the stranger joining through the invitation', stepLimit(8));
  report.joined = await person(stranger.tab);
  console.log(`ok: the invitation brought the stranger in (${report.joined.room})`);
  mark('the invitation brought the stranger into the workspace');

  // The page hands a person it let in by a link, and who is not set up here,
  // to the site itself, where the first screen is: an agent sets Tilo up, or
  // they skip it. This person has an agent.
  await until(stranger.tab, firstScreenShown,
    'the first screen of a person who is not set up yet', stepLimit(8));
  report.firstScreenShown = await stranger.tab.evaluate(firstScreenShown);
  if (!report.firstScreenShown) await fail('the first screen is in the page but not shown');
  await until(stranger.tab, `['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the site arriving behind the first screen', stepLimit(6));
  report.firstScreen = await stranger.tab.evaluate(`({
    heading: document.getElementById('first-run-heading')?.textContent?.trim() ?? null,
    prompt: document.getElementById('first-run-offer')?.textContent?.trim() ?? null,
    skip: document.getElementById('first-run-skip')?.textContent?.trim() ?? null,
    manual: document.getElementById('first-run-manual-toggle')?.textContent?.trim() ?? null,
  })`);
  if (!report.firstScreen.skip) await fail('the first screen offers no way past it for a person without an agent');
  await until(stranger.tab, `document.getElementById('first-run')?.dataset.setup === 'ready'`, 'the setup prompt made', stepLimit(6));
  const beforeCopy = await stranger.tab.evaluate(`navigator.clipboard.readText().catch(() => '')`).catch(() => '');
  await stranger.tab.send('Browser.grantPermissions', {origin: siteOrigin, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  await pressId(stranger.tab, 'first-run-agent');
  await until(stranger.tab, `document.documentElement.dataset.firstRun === 'waiting'`, 'the first screen answering the click', stepLimit(4));
  const prompt = await copied(stranger.tab, beforeCopy);
  report.prompt = prompt;
  const setup = commandOf(prompt);
  if (!setup) await fail(`the prompt names no tilo login code: ${prompt}`);
  const env = await agentEnv();
  const login = await tilo(env, ['login', setup.code, '--site', setup.site ?? site]);
  report.login = login;
  if (login.code !== 0) await fail(`tilo login failed (${login.code}): ${login.stderr}${login.stdout}`);
  const registered = (() => { try { return readFileSync(claudeCalls, 'utf8'); } catch { return ''; } })();
  report.claude = registered;
  if (!/mcp add .*tilo .*-- \S*tilo mcp/.test(registered)) await fail(`tilo login did not add its MCP server to the agent: ${registered || 'never asked'}`);
  console.log('ok: tilo login ran and added Tilo’s MCP server to the agent');
  await until(stranger.tab, `!${firstScreenShown}`, 'the first screen leaving', stepLimit(4));
  for (const deadline = Date.now() + stepLimit(3); !await ownWorkspace(stranger.tab).catch(() => '');) {
    if (Date.now() > deadline) await fail(`the tab is titled "${await stranger.tab.evaluate('document.title')}", not after the person's own workspace`);
    await sleep(250);
  }
  report.workspace = await ownWorkspace(stranger.tab);
  report.landed = await person(stranger.tab);
  await dump(stranger.tab, '5-own-workspace');
  mark('tilo login ran and the tab is in the person\'s own workspace');
  console.log(`ok: the person is in their own workspace (${report.workspace})`);

  // -- 4. a passkey brings the same person back to that workspace ------------
  // Kept from the person's own desktop, as "Keep with passkey" does.
  const kept = await stranger.tab.evaluate(`(async () => {
    const id = ${identityModule};
    const person = await id.browserIdentity();
    try {
      const envelope = await person.keepWithPasskey({userName: 'wait list passkey'});
      // Where the passkey signs in from: the hosts that know this person keep
      // the envelope their passkey opens, as "Keep your name and work" does.
      await id.publishEnvelope({origin: ${JSON.stringify(hostOrigin)}, identity: person});
      return JSON.stringify({person: person.userPublicKey, wraps: (envelope?.wraps ?? []).filter(wrap => wrap.kind === 'passkey').length});
    } catch (error) { return JSON.stringify({error: String(error?.message ?? error), code: error?.code ?? error?.name}); }
  })()`).then(JSON.parse).catch(error => ({error: error.message}));
  report.passkey = kept;
  if (kept.error) await fail(`the passkey was not registered: ${kept.error} (${kept.code})`);
  if (kept.person !== report.landed.user) await fail(`the passkey was made for ${kept.person}, not the person in the workspace`);
  // The browser forgets the site, as a person who cleared it, or one on a
  // computer that never had it: the authenticator alone remembers them.
  await stranger.tab.send('Storage.clearDataForOrigin', {origin: siteOrigin, storageTypes: 'all'});
  await arrive(stranger, {expected: 'refused'});
  await until(stranger.tab, `document.documentElement.dataset.door === 'wait-list'`, 'the door after the browser forgot the site', stepLimit(6));
  await pressId(stranger.tab, 'arrival-sign-in-button');
  const back = await until(stranger.tab, `(['done', 'stopped'].includes(document.documentElement.dataset.arrival) && !document.documentElement.dataset.door) ? 'in' : (document.getElementById('arrival-sign-in-note')?.textContent?.trim() || '')`,
    'the passkey signing the person in', stepLimit(10));
  if (back !== 'in') await fail(`the passkey did not sign the person in: ${back}`);
  await until(stranger.tab, `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')[${JSON.stringify(hostOrigin)}]?.userPublicKey`,
    'the person back in a workspace', stepLimit(8));
  const returned = await person(stranger.tab);
  report.returned = returned;
  if (returned.user !== kept.person) await fail(`the passkey signed in ${returned.user}, not ${kept.person}`);
  await until(stranger.tab, `(() => { const t = document.title; return /^Tilo · /.test(t) && !/lounge/i.test(t); })()`,
    "the person back in their own workspace", stepLimit(6));
  report.returnedTo = await ownWorkspace(stranger.tab);
  await dump(stranger.tab, '6-passkey-back');
  console.log(`ok: a passkey signed the same person back in, to ${report.returnedTo}`);
  mark('a passkey signed the person back in to their own workspace');

  // -- 5. a second workspace -------------------------------------------------
  await search(stranger.tab, 'new workspace', 'New workspace');
  await until(stranger.tab, `document.querySelector('.tilo-entry-card input') !== null`, 'the card asking what to name it', stepLimit(6));
  const named = 'Studio';
  await stranger.tab.evaluate(`(() => {
    const input = document.querySelector('.tilo-entry-card input');
    input.value = ${JSON.stringify(named)};
    input.dispatchEvent(new Event('input', {bubbles: true}));
  })()`);
  await pressText(stranger.tab, 'Create', '.tilo-entry-card');
  await until(stranger.tab, `document.title.includes(${JSON.stringify(named)})`, 'the second workspace opening', stepLimit(10));
  report.secondWorkspace = await stranger.tab.evaluate(`document.title`);
  await dump(stranger.tab, '7-second-workspace');
  console.log(`ok: the person made and opened a second workspace (${report.secondWorkspace})`);
  mark('the person made a second workspace');

  // -- 6. a visitor with no invitation is still out --------------------------
  const visitor = await visit('visitor', {passkey: false});
  await arrive(visitor, {expected: 'refused'});
  await until(visitor.tab, `document.documentElement.dataset.door === 'wait-list'`, 'the last visitor meeting the wait list', stepLimit(6));
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
  const third = await waitForMail('the last visitor\'s confirmation mail', 3);
  report.lastMail = {to: third.to, subject: third.subject};
  await dump(visitor.tab, '8-without-an-invitation');
  console.log(`ok: a visitor with no invitation is still on the wait list (${third.to})`);
  mark('a visitor with no invitation left the wait list and got no further');
  await finished();
} catch (error) {
  await fail(error?.stack ? `${error.message}\n${error.stack}` : String(error));
}
