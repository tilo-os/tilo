#!/usr/bin/env node
// Getting in, end to end, on the stack production runs: the web entry (the
// desktop bundle laid out as the web entry image serves it) and the room
// service in persistent mode with its child relay, both started by
// `dev/run.py` from the binaries and bundle given here, and headless Chrome
// through tools/web/chrome.mjs, each person a browser context of their own.
//
// usage: node crates/hosting/tilo-room-service/dev/auth-e2e.mjs --bundle DIR
//          --relay-bin PATH --room-service-bin PATH --out DIR
//          [--expect-revision SHA] [--allow-dev] [--timeout S]
//
// It checks, in order (a person is a fresh browser context; their browser is
// the context, their tabs its pages):
//   first-use      a fresh profile opens the bare entry: one POST /join at the
//                  host's door, the desktop presents, and the browser keeps
//                  the membership (host, room, person) and the room's link
//   reload         the same tab reloaded: the desktop presents again as the
//                  same person in the same room, with no new join
//   second-tab     a second tab of that profile while the first holds its
//                  device key: no new join, the membership kept, and the tab
//                  either opens or says the workspace is open in another tab
//   new-tab        every tab closed, then the room's link in a new tab of the
//                  same profile: presents as the same person, no new join
//   damaged-link   a link whose invitation secrets are out of order is refused
//                  as damaged, before anything is asked of the host
//   stolen-invite  another person's browser opens the first person's
//                  invitation and one-time token (spent, and bound to the
//                  first person's key): refused, nothing kept, no desktop
//   bound-invite   the same with an invitation the door issued for someone
//                  else who has not used it yet: refused when redeemed,
//                  nothing kept
//   host-reset     the host is set up again (run.py --fresh: a new root, relay
//                  and room) while the first person's browser keeps what it
//                  had for the old one: the bare entry is refused, saying the
//                  host changed, without a join, and the kept membership and
//                  person stay (the browser's state is pinned to the old host
//                  and is never handed to the new one)
//   stale-link     the old room's link after the reset: refused the same way,
//                  never opened as another room
//
// Every check runs and is reported (summary.json, summary.md under --out,
// with each page's console and a screenshot of each failure); the run exits 1
// when any failed. It exits 2 when it cannot observe its subject: the bundle
// differs from its build.json (or is not a clean release build of
// --expect-revision, unless --allow-dev), the stack did not come up, or Chrome
// cannot draw WebGPU. Credentials are all made here, by the pages and run.py;
// nothing reaches a deployed host.
import {spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdirSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Invalid, chromeFlags, verifyBundle, within} from '../../../../scripts/perf/browser_qualification.mjs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const RUN_PY = join(HERE, 'run.py');
const MEMBERSHIPS = 'tilo-host-memberships';
const USAGE = 'usage: auth-e2e.mjs --bundle DIR --relay-bin PATH --room-service-bin PATH --out DIR '
  + '[--expect-revision SHA] [--allow-dev] [--timeout S]';
/** The states an entry page settles in (`data-entry`, entry-page.mjs). */
export const SETTLED = ['open', 'refused', 'failed', 'unsupported'];

export function parseArgs(argv) {
  const options = {allowDev: false, timeout: 300};
  const paths = {'--bundle': 'bundle', '--relay-bin': 'relayBin', '--room-service-bin': 'roomServiceBin', '--out': 'out'};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg in paths) {
      if (argv[i + 1] === undefined) throw Error(`${arg} needs a value\n${USAGE}`);
      options[paths[arg]] = resolve(argv[++i]);
    } else if (arg === '--expect-revision') options.expectRevision = argv[++i];
    else if (arg === '--allow-dev') options.allowDev = true;
    else if (arg === '--timeout') {
      options.timeout = Number(argv[++i]);
      if (!Number.isFinite(options.timeout) || options.timeout <= 0) throw Error('--timeout needs a positive number of seconds');
    } else throw Error(`unknown argument ${arg}\n${USAGE}`);
  }
  for (const name of Object.values(paths)) if (!options[name]) throw Error(USAGE);
  return options;
}

/** run.py's banner: the room it hosts. */
export function parseBanner(text) {
  const match = /Tilo is up in [\d.]+s, room '[^']*' \(([0-9a-f-]{36})\)/.exec(text);
  return match ? {room: match[1]} : null;
}

/** The membership `memberships` (localStorage's JSON) keeps for `origin`, or `null`. */
export function membershipFor(memberships, origin) {
  let all;
  try { all = JSON.parse(memberships ?? '{}') ?? {}; } catch { return null; }
  return all[new URL(origin).origin] ?? null;
}

/** The POST /join fetches among `requests` (`{url, method}`) to the door at `api`. */
export function joinsTo(requests, api) {
  const door = new URL('join', `${api}/`).href;
  return requests.filter(request => request.method === 'POST' && request.url === door);
}

/** Whether `port` on 127.0.0.1 can be listened on now. */
const bindable = port => new Promise(done => {
  const probe = createServer();
  probe.once('error', () => done(false));
  probe.listen(port, '127.0.0.1', () => probe.close(() => done(true)));
});

/** A free TCP port on 127.0.0.1 (and, with `span`, the port `span` above
 * it), below every system's ephemeral range: run.py probes its listeners
 * while they start, and a connection to an unbound ephemeral port can meet
 * itself (TCP simultaneous open) and read its own request back. */
async function freePort(taken, span = 0) {
  for (;;) {
    const port = 20000 + Math.floor(Math.random() * (32000 - 20000 - span));
    if (taken.has(port) || taken.has(port + span)) continue;
    if (await bindable(port) && (!span || await bindable(port + span))) {
      taken.add(port).add(port + span);
      return port;
    }
  }
}

/** run.py with `ports`, until its banner; resolves to `{room, stop}`. */
async function startStack(options, ports, {fresh}) {
  const args = [RUN_PY, '--scratch', options.scratch, '--bundle', options.bundle,
    '--relay-bin', options.relayBin, '--room-service-bin', options.roomServiceBin, '--name', 'auth',
    '--web-port', String(ports.web), '--api-port', String(ports.api), '--port-base', String(ports.base),
    ...(fresh ? ['--fresh'] : [])];
  const child = spawn('python3', args, {stdio: ['ignore', 'pipe', 'pipe']});
  let output = '';
  const exited = new Promise(done => child.once('exit', code => done(code)));
  const banner = new Promise((done, fail) => {
    const read = chunk => {
      output += chunk;
      const found = parseBanner(output);
      if (found) done(found);
    };
    child.stdout.on('data', read);
    child.stderr.on('data', chunk => { output += chunk; process.stderr.write(chunk); });
    exited.then(code => fail(new Invalid(`run.py exited (${code}) before the room came up:\n${output.slice(-2000)}`)));
  });
  const {room} = await within(banner, 180_000, 'run.py');
  const stop = async () => {
    if (child.exitCode !== null) return;
    // run.py stops the service, which closes its relay's store, on SIGTERM.
    child.kill('SIGTERM');
    const code = await Promise.race([exited, sleep(30_000).then(() => 'timeout')]);
    if (code === 'timeout') child.kill('SIGKILL');
  };
  return {room, stop, log: () => output};
}

// Every fetch a page makes, told to the harness through a DevTools binding
// (which outlives reloads), with the body of the door's answers. DevTools'
// own network domain held up later pages of the run.
const FETCHES = `(() => {
  const original = globalThis.fetch;
  const tell = entry => { try { globalThis.__authE2eFetch(JSON.stringify(entry)); } catch {} };
  globalThis.fetch = async function (input, init) {
    const url = new URL(input instanceof Request ? input.url : String(input), location.href).href;
    const method = String(init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
    try {
      const response = await original.call(this, input, init);
      const entry = {url, method, status: response.status};
      if (method === 'POST' && new URL(url).pathname.endsWith('/join')) entry.body = await response.clone().text().catch(() => null);
      tell(entry);
      return response;
    } catch (error) {
      tell({url, method, error: String(error)});
      throw error;
    }
  };
})();`;

/** A page of `browser` in `context`, with the fetches it makes recorded. */
async function page(browser, context) {
  const tab = await browser.tab({context});
  const requests = [];
  const off = browser.on(message => {
    if (message.sessionId !== tab.sessionId || message.method !== 'Runtime.bindingCalled') return;
    if (message.params.name !== '__authE2eFetch') return;
    try { requests.push(JSON.parse(message.params.payload)); } catch { /* Not one of ours. */ }
  });
  await tab.send('Runtime.addBinding', {name: '__authE2eFetch'});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FETCHES});
  await tab.send('Emulation.setFocusEmulationEnabled', {enabled: true});
  return {...tab, requests, close: async () => { off(); await tab.close(); }};
}

/** What the page shows once it settled: its entry state, status line, link
 * and the membership it keeps for the host at `relay`. */
async function settle(tab, {relay, timeout}) {
  // A page that never settles is that check's failure, reported with what
  // it showed by then; the run goes on to the next check.
  const state = await tab.waitFor(`(() => {
    const root = document.documentElement.dataset;
    return !globalThis.__authE2eLeft && ${JSON.stringify(SETTLED)}.includes(root.entry) ? root.entry : null;
  })()`, {timeout}).catch(() => null);
  if (state === 'open') {
    // The desktop drew its first frame (`data-arrival` done); SwiftShader
    // compiles its pipelines on the first one.
    await tab.waitFor("document.documentElement.dataset.arrival === 'done'", {timeout}).catch(() => {});
  }
  return tab.evaluate(`(() => ({
    entry: globalThis.__authE2eLeft ? 'not loaded' : document.documentElement.dataset.entry ?? 'unsettled',
    phase: document.documentElement.dataset.phase ?? null,
    arrival: document.documentElement.dataset.arrival ?? null,
    status: document.getElementById('status')?.textContent ?? '',
    hash: location.hash,
    membership: (${membershipFor})(localStorage.getItem(${JSON.stringify(MEMBERSHIPS)}), ${JSON.stringify(relay)}),
    // Where a page that never settled stood: still loading its modules, or past them.
    loading: document.documentElement.dataset.entry ? undefined : {readyState: document.readyState,
      resources: performance.getEntriesByType('resource').length,
      unfinished: performance.getEntriesByType('resource').filter(r => !r.responseEnd).map(r => r.name).slice(0, 5)},
  }))()`);
}

/** Marks the current document, so `settle` waits for the next one. */
const leaving = tab => tab.evaluate('globalThis.__authE2eLeft = true').catch(() => {});

/** Navigate `tab` to `url` (a fresh tab) and settle. */
async function open(tab, url, settleOptions) {
  await tab.navigate(url);
  return settle(tab, settleOptions);
}

/** Reload `tab` and settle. */
async function reload(tab, settleOptions) {
  await leaving(tab);
  await tab.reload();
  return settle(tab, settleOptions);
}

/** Press the entry card's button labelled `label` once it shows; resolves
 * to whether it did, `false` when the page settled without offering it. */
async function press(tab, label, timeout) {
  const button = `[...document.querySelectorAll('.tilo-entry-card button')].find(b => b.textContent === ${JSON.stringify(label)})`;
  const seen = await tab.waitFor(`${button} ? 'card' : (${JSON.stringify(SETTLED)}.includes(document.documentElement.dataset.entry) ? 'settled' : null)`,
    {timeout});
  if (seen !== 'card') return false;
  await tab.evaluate(`${button}.click()`);
  return true;
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exit(2);
  }
  mkdirSync(options.out, {recursive: true});
  // run.py's binaries, site and room state: not part of the report.
  options.scratch = mkdtempSync(join(tmpdir(), 'tilo-auth-e2e-'));
  const bundle = verifyBundle(options.bundle, {allowDev: options.allowDev, expectRevision: options.expectRevision});
  const timeout = options.timeout * 1000;
  const taken = new Set();
  const ports = {web: await freePort(taken), api: await freePort(taken), base: await freePort(taken, 64)};
  const web = `http://localhost:${ports.web}`;
  const api = `http://localhost:${ports.api}`;
  const relay = `http://localhost:${ports.base + 64}`;
  const host = new URL(relay).host;
  const settleOptions = {relay, timeout};
  const checks = [];
  const pages = [];
  const failed = [];
  const check = (name, ok, detail) => {
    checks.push({name, ok: Boolean(ok), detail});
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${JSON.stringify(detail)}`);
    if (!ok) failed.push(name);
  };
  const shoot = async (name, tab) => {
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(options.out, `${name}.png`), shot);
  };
  const keepLog = (name, tab) => pages.push({name, log: tab.log, requests: tab.requests.map(({url, method, status, error}) => ({url, method, status, error}))});

  let stack = await startStack(options, ports, {fresh: true});
  // Chrome's own WebGPU on the CPU, as the qualification runs it: CI runners
  // have no GPU, and a host GPU in headless Chrome can lose its device mid-run.
  const browser = await launch({args: chromeFlags('swiftshader', process.getuid?.())});
  try {
    const roomLink = room => `${web}/#/@${host}/w/${room}`;
    const firstRoom = stack.room;

    // first-use
    const person = await browser.context();
    const first = await page(browser, person);
    const opened = await open(first, `${web}/`, settleOptions);
    keepLog('first-use', first);
    if (opened.entry === 'unsupported') throw new Invalid('Chrome cannot draw WebGPU here: the entry page refused the renderer');
    const joins = joinsTo(first.requests, api);
    const me = opened.membership?.userPublicKey ?? null;
    check('first-use', opened.entry === 'open' && opened.arrival === 'done' && joins.length === 1 && joins[0].status === 200
      && opened.membership?.room === firstRoom && /^[0-9a-f]{64}$/.test(me ?? '') && opened.hash.includes(`/w/${firstRoom}`),
    {entry: opened.entry, arrival: opened.arrival, joins: joins.map(j => j.status), membership: opened.membership, hash: opened.hash,
      status: opened.status, loading: opened.loading});
    if (opened.entry !== 'open') await shoot('first-use', first);
    const offer = joins[0]?.status === 200 && joins[0].body ? JSON.parse(joins[0].body) : null;

    // reload
    const before = first.requests.length;
    const reloaded = await reload(first, settleOptions);
    let newJoins = joinsTo(first.requests.slice(before), api);
    check('reload', reloaded.entry === 'open' && reloaded.arrival === 'done' && newJoins.length === 0
      && reloaded.membership?.userPublicKey === me && reloaded.membership?.room === firstRoom,
    {entry: reloaded.entry, arrival: reloaded.arrival, joins: newJoins.length, membership: reloaded.membership, status: reloaded.status, loading: reloaded.loading});
    if (reloaded.entry !== 'open') await shoot('reload', first);

    // second-tab: the first still holds the profile's device key.
    const second = await page(browser, person);
    const beside = await open(second, `${web}/`, settleOptions);
    keepLog('second-tab', second);
    newJoins = joinsTo(second.requests, api);
    // One tab holds a profile's device key (device-custody.mjs): the second
    // either opens or says where the workspace already is.
    check('second-tab', (beside.entry === 'open' || /another tab/.test(beside.status)) && newJoins.length === 0
      && beside.membership?.userPublicKey === me && beside.membership?.room === firstRoom,
      {entry: beside.entry, joins: newJoins.length, membership: beside.membership, status: beside.status});
    if (beside.entry !== 'open') await shoot('second-tab', second);

    // new-tab: the room's link, in a new tab once no other holds the key.
    await first.close();
    await second.close();
    const third = await page(browser, person);
    const again = await open(third, roomLink(firstRoom), settleOptions);
    keepLog('new-tab', third);
    newJoins = joinsTo(third.requests, api);
    check('new-tab', again.entry === 'open' && again.arrival === 'done' && newJoins.length === 0
      && again.membership?.userPublicKey === me && again.membership?.room === firstRoom,
    {entry: again.entry, joins: newJoins.length, membership: again.membership, status: again.status});
    if (again.entry !== 'open') await shoot('new-tab', third);
    await third.close();

    // damaged-link: secrets out of grammar order.
    const stranger = await browser.context();
    const damaged = await page(browser, stranger);
    const refusedDamaged = await open(damaged, `${roomLink(firstRoom)}?token=x&invite=y`, settleOptions);
    keepLog('damaged-link', damaged);
    check('damaged-link', refusedDamaged.entry === 'refused' && /damaged/.test(refusedDamaged.status)
      && !refusedDamaged.hash.includes('token=') && refusedDamaged.membership === null
      && damaged.requests.every(request => !request.url.startsWith(relay) && !request.url.startsWith(api)),
    {entry: refusedDamaged.entry, status: refusedDamaged.status, hash: refusedDamaged.hash});
    await damaged.close();

    // An invitation and one-time token the door issued for one person, opened
    // in another person's browser: refused when redeemed, nothing kept.
    const redeemElsewhere = async (name, issued) => {
      const thief = await page(browser, stranger);
      let shown = null;
      if (issued) {
        await thief.navigate(`${roomLink(firstRoom)}?invite=${issued.invitation}&token=${issued.admissionToken}`);
        // A genuine invitation the host still honours is offered first.
        const offered = await press(thief, 'Join', timeout);
        shown = {...await settle(thief, settleOptions), offered};
      }
      keepLog(name, thief);
      const kept = membershipFor(await thief.evaluate(`localStorage.getItem(${JSON.stringify(MEMBERSHIPS)})`).catch(() => null), relay);
      check(name, shown?.entry === 'refused' && !shown.hash.includes('token=') && kept === null
        && joinsTo(thief.requests, api).length === 0,
      {entry: shown?.entry ?? 'no invitation issued', offered: shown?.offered, status: shown?.status, membership: kept});
      if (shown?.entry !== 'refused') await shoot(name, thief);
      await thief.close();
    };
    // stolen-invite: the first person's, already spent by them.
    await redeemElsewhere('stolen-invite', offer);
    // bound-invite: one the door issued for someone who has not used it yet.
    const someoneElse = createHash('sha256').update(`someone else ${Date.now()}`).digest('hex');
    const issued = await fetch(`${api}/join`, {method: 'POST', headers: {'content-type': 'application/json'},
      body: JSON.stringify({userPublicKey: someoneElse, displayName: 'Someone Else'})})
      .then(response => (response.ok ? response.json() : null)).catch(() => null);
    await redeemElsewhere('bound-invite', issued);

    // host-reset: a new root, relay and room behind the same origin.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: true});
    const changed = /set up again/;
    const reset = await page(browser, person);
    const refusedReset = await open(reset, `${web}/`, settleOptions);
    keepLog('host-reset', reset);
    check('host-reset', stack.room !== firstRoom && refusedReset.entry === 'refused' && changed.test(refusedReset.status)
      && joinsTo(reset.requests, api).length === 0 && refusedReset.membership?.userPublicKey === me
      && refusedReset.membership?.room === firstRoom,
    {entry: refusedReset.entry, joins: joinsTo(reset.requests, api).length, membership: refusedReset.membership,
      room: stack.room, status: refusedReset.status, loading: refusedReset.loading});
    if (refusedReset.entry !== 'refused') await shoot('host-reset', reset);
    await reset.close();

    // stale-link: the old room's link after the reset.
    const stale = await page(browser, person);
    const staleOpened = await open(stale, roomLink(firstRoom), settleOptions);
    keepLog('stale-link', stale);
    check('stale-link', staleOpened.entry === 'refused' && joinsTo(stale.requests, api).length === 0
      && staleOpened.membership?.userPublicKey === me,
    {entry: staleOpened.entry, joins: joinsTo(stale.requests, api).length, status: staleOpened.status,
      membership: staleOpened.membership});
    if (staleOpened.entry !== 'refused') await shoot('stale-link', stale);
    await stale.close();
  } finally {
    await browser.close().catch(error => console.error(`closing Chrome: ${error.message}`));
    await stack.stop();
    writeFileSync(join(options.out, 'stack.log'), stack.log());
    writeFileSync(join(options.out, 'pages.json'), `${JSON.stringify(pages, null, 2)}\n`);
    rmSync(options.scratch, {recursive: true, force: true});
  }
  const summary = {revision: bundle.revision, dirty: bundle.dirty, profile: bundle.profile, runtimeSha256: bundle.runtimeSha256,
    checks, passed: failed.length === 0};
  writeFileSync(join(options.out, 'summary.json'), `${JSON.stringify(summary, null, 2)}\n`);
  writeFileSync(join(options.out, 'summary.md'), [
    `# Browser authentication, end to end (${bundle.revision}${bundle.dirty ? ', dirty' : ''}, ${bundle.profile})`, '',
    '| check | result | detail |', '|---|---|---|',
    ...checks.map(c => `| ${c.name} | ${c.ok ? 'ok' : 'FAIL'} | \`${JSON.stringify(c.detail).replaceAll('|', '\\|')}\` |`), ''].join('\n'));
  if (failed.length) {
    console.error(`auth-e2e: ${failed.length} of ${checks.length} checks failed: ${failed.join(', ')}`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => {
    console.error(error instanceof Invalid ? `auth-e2e: invalid run: ${error.message}` : error);
    process.exit(error instanceof Invalid ? 2 : 1);
  });
}
