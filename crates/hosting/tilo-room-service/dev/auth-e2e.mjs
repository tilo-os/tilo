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
//                  device key: no new join and the membership kept, whatever
//                  the tab shows
//   new-tab        every tab closed, then the room's link in a new tab of the
//                  same profile: presents as the same person, no new join
//   damaged-link   a link whose invitation secrets are out of order is refused
//                  as damaged, before anything is asked of the host
//   stolen-invite  another person's browser opens the first person's
//                  invitation and one-time token (spent, and bound to the
//                  first person's key): refused, nothing kept, no desktop
//   host-reset     the host forgets the room (run.py --fresh: new root, new
//                  room) while the first person's browser still holds the old
//                  membership: the bare entry recovers through the door as the
//                  same person, into the new room, with one new join
//   stale-link     the old room's link after the reset: refused as no access,
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
import {mkdirSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Invalid, verifyBundle, within} from '../../../../scripts/perf/browser_qualification.mjs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const RUN_PY = join(HERE, 'run.py');
const MEMBERSHIPS = 'tilo-host-memberships';
const USAGE = 'usage: auth-e2e.mjs --bundle DIR --relay-bin PATH --room-service-bin PATH --out DIR '
  + '[--expect-revision SHA] [--allow-dev] [--timeout S]';
// Chrome's own WebGPU on the CPU: CI runners have no GPU, and a host GPU in
// headless Chrome can lose its device mid-run.
const CHROME_ARGS = ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader'];
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

/** The POST /join requests among `requests` (`{url, method}`) to the door at `api`. */
export function joinsTo(requests, api) {
  const door = new URL('join', `${api}/`).href;
  return requests.filter(request => request.method === 'POST' && request.url === door);
}

/** A free TCP port on 127.0.0.1 (and, with `span`, the port `span` above it). */
async function freePort(span = 0) {
  for (;;) {
    const port = await new Promise((done, fail) => {
      const server = createServer();
      server.once('error', fail);
      server.listen(0, '127.0.0.1', () => { const {port: chosen} = server.address(); server.close(() => done(chosen)); });
    });
    if (!span) return port;
    if (port + span < 65535 && await new Promise(done => {
      const probe = createServer();
      probe.once('error', () => done(false));
      probe.listen(port + span, '127.0.0.1', () => probe.close(() => done(true)));
    })) return port;
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

/** A page of `browser` in `context`, with the requests it makes recorded. */
async function page(browser, context) {
  const tab = await browser.tab({context});
  const requests = [];
  const off = browser.on(message => {
    if (message.sessionId !== tab.sessionId) return;
    if (message.method === 'Network.requestWillBeSent') {
      const {requestId, request} = message.params;
      requests.push({id: requestId, url: request.url, method: request.method});
    } else if (message.method === 'Network.responseReceived') {
      const entry = requests.findLast(candidate => candidate.id === message.params.requestId);
      if (entry) entry.status = message.params.response.status;
    }
  });
  await tab.send('Network.enable');
  await tab.send('Emulation.setFocusEmulationEnabled', {enabled: true});
  return {
    ...tab,
    requests,
    /** The body of the recorded response `request`. */
    body: async request => {
      const reply = await tab.send('Network.getResponseBody', {requestId: request.id});
      if (!reply.result) throw Error(`no body for ${request.url}: ${reply.error?.message}`);
      return reply.result.base64Encoded ? Buffer.from(reply.result.body, 'base64').toString() : reply.result.body;
    },
    close: async () => { off(); await tab.close(); },
  };
}

/** What the page shows once it settled: its entry state, status line, link
 * and the membership it keeps for the host at `relay`. */
async function settle(tab, {relay, timeout}) {
  const state = await tab.waitFor(`(() => {
    const root = document.documentElement.dataset;
    return !globalThis.__authE2eLeft && ${JSON.stringify(SETTLED)}.includes(root.entry) ? root.entry : null;
  })()`, {timeout});
  if (state === 'open') {
    // The desktop drew its first frame (`data-arrival` done); SwiftShader
    // compiles its pipelines on the first one.
    await tab.waitFor("document.documentElement.dataset.arrival === 'done'", {timeout}).catch(() => {});
  }
  return tab.evaluate(`(() => ({
    entry: document.documentElement.dataset.entry ?? null,
    arrival: document.documentElement.dataset.arrival ?? null,
    status: document.getElementById('status')?.textContent ?? '',
    hash: location.hash,
    membership: (${membershipFor})(localStorage.getItem(${JSON.stringify(MEMBERSHIPS)}), ${JSON.stringify(relay)}),
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
  const ports = {web: await freePort(), api: await freePort(), base: await freePort(64)};
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
  const keepLog = (name, tab) => pages.push({name, log: tab.log, requests: tab.requests.map(({url, method, status}) => ({url, method, status}))});

  let stack = await startStack(options, ports, {fresh: true});
  const browser = await launch({args: CHROME_ARGS});
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
      status: opened.status});
    if (opened.entry !== 'open') await shoot('first-use', first);
    const offer = joins[0]?.status === 200 ? JSON.parse(await first.body(joins[0])) : null;

    // reload
    const before = first.requests.length;
    const reloaded = await reload(first, settleOptions);
    let newJoins = joinsTo(first.requests.slice(before), api);
    check('reload', reloaded.entry === 'open' && reloaded.arrival === 'done' && newJoins.length === 0
      && reloaded.membership?.userPublicKey === me && reloaded.membership?.room === firstRoom,
    {entry: reloaded.entry, arrival: reloaded.arrival, joins: newJoins.length, membership: reloaded.membership, status: reloaded.status});
    if (reloaded.entry !== 'open') await shoot('reload', first);

    // second-tab: the first still holds the profile's device key.
    const second = await page(browser, person);
    const beside = await open(second, `${web}/`, settleOptions);
    keepLog('second-tab', second);
    newJoins = joinsTo(second.requests, api);
    check('second-tab', newJoins.length === 0 && beside.membership?.userPublicKey === me && beside.membership?.room === firstRoom,
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

    // stolen-invite: the first person's invitation and token, in another browser.
    const thief = await page(browser, stranger);
    let stolen = null;
    if (offer) {
      await thief.navigate(`${roomLink(firstRoom)}?invite=${offer.invitation}&token=${offer.admissionToken}`);
      // The invitation itself is genuine, so the page offers it first.
      const offered = await press(thief, 'Join', timeout);
      stolen = {...await settle(thief, settleOptions), offered};
    }
    keepLog('stolen-invite', thief);
    const memberships = await thief.evaluate(`localStorage.getItem(${JSON.stringify(MEMBERSHIPS)})`).catch(() => null);
    check('stolen-invite', stolen && stolen.entry !== 'open' && SETTLED.includes(stolen.entry) && !stolen.hash.includes('token=')
      && membershipFor(memberships, relay) === null && joinsTo(thief.requests, api).length === 0,
    {entry: stolen?.entry ?? 'no invitation captured', offered: stolen?.offered, status: stolen?.status, membership: membershipFor(memberships, relay)});
    if (stolen?.entry === 'open') await shoot('stolen-invite', thief);
    await thief.close();

    // host-reset: a new root and a new room behind the same host.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: true});
    const recovering = await page(browser, person);
    const recovered = await open(recovering, `${web}/`, settleOptions);
    keepLog('host-reset', recovering);
    newJoins = joinsTo(recovering.requests, api);
    check('host-reset', stack.room !== firstRoom && recovered.entry === 'open' && recovered.arrival === 'done'
      && newJoins.length === 1 && newJoins[0].status === 200 && recovered.membership?.userPublicKey === me
      && recovered.membership?.room === stack.room && recovered.hash.includes(`/w/${stack.room}`),
    {entry: recovered.entry, joins: newJoins.map(j => j.status), membership: recovered.membership, room: stack.room,
      status: recovered.status});
    if (recovered.entry !== 'open') await shoot('host-reset', recovering);
    await recovering.close();

    // stale-link: the old room's link after the reset.
    const stale = await page(browser, person);
    const staleOpened = await open(stale, roomLink(firstRoom), settleOptions);
    keepLog('stale-link', stale);
    check('stale-link', staleOpened.entry === 'refused' && staleOpened.membership?.userPublicKey === me,
      {entry: staleOpened.entry, status: staleOpened.status, membership: staleOpened.membership});
    if (staleOpened.entry === 'open') await shoot('stale-link', stale);
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
