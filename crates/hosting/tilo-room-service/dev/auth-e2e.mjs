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
//   stale-link     the host is set up again (run.py --fresh: a new root, relay
//                  and room) while the first person's browser keeps what it
//                  had for the old one; the old room's link is refused without
//                  a join and without offering to join again (that room is
//                  gone), never opened as another room, the membership kept
//   host-declined  the bare entry offers to join the new host again; "Not now"
//                  is refused, saying the host was set up again, without a
//                  join, and the kept membership and person stay
//   host-reset     the same offer accepted ("Join again"): one join at the
//                  door, the desktop presents, and the membership names the
//                  new room as the same person
//   host-reload    that tab reloaded: presents again with no new join
//   kept-apart     what the browser kept for the old host is untouched (every
//                  row of its namespace as before the reset), and the new
//                  host's state lives in a namespace of its own, certified by
//                  the new host: the old state was never reused or handed to it
//   stale-rejoined the old room's link once joined again: refused without a
//                  join, the new membership kept
//   pruned-membership
//                  the host started again (not anew) with a one-minute prune
//                  horizon (TILO_ROOMS_PRUNE_AFTER_MINS), and the person away
//                  past it and its next prune: the host no longer has them.
//                  The bare entry joins at the door exactly once, and the
//                  desktop presents as the same person in the same room
//   removed-member the host started again with the person as one of the
//                  room's moderators (TILO_ROOMS_MODERATORS: a moderator
//                  removes the guests ranked below them); a second person
//                  joins, and the moderator removes them through the
//                  desktop's own door (the room's remove-member action,
//                  confirmed). The removed person's
//                  page makes no join while open; reloaded, it is refused,
//                  saying they may have been removed, with no desktop and at
//                  most one join, which the door turns away
//   removed-entry  the removed person opens the bare entry: refused, saying
//                  they were removed and that an invitation brings them back,
//                  with no desktop, one join the door turns away, and the
//                  membership no longer kept
//   cooling-down   a fresh profile from the removed person's address is
//                  refused at the door, saying why, with no desktop
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
/** Where a profile keeps its peer (peer.mjs): the namespace it starts in, and
 * the localStorage map of the namespace each profile moved to since. */
const PROFILE_DEVICE = 'tilo-browser-device';
const PROFILE_DEVICES = 'tilo-profile-devices';
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

const hexKey = key => (typeof key === 'string' ? key : Array.from(key ?? [], byte => byte.toString(16).padStart(2, '0')).join(''));

/**
 * What a browser keeps in each namespace of one profile, from the rows
 * `KEPT_ROWS` reads (`{device, key, digest, certificate}`): `{[device]:
 * {rows, digest, hostDevice}}`, where `digest` changes with any row of the
 * namespace and `hostDevice` (hex) names the host its certificate is from.
 */
export function keptNamespaces(rows) {
  const namespaces = {};
  const hashes = {};
  for (const row of [...rows].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0))) {
    const kept = namespaces[row.device] ??= {rows: 0, digest: null, hostDevice: null};
    (hashes[row.device] ??= createHash('sha256')).update(`${row.key}\u0000${row.digest}\u0000`);
    kept.rows += 1;
    if (row.certificate) {
      try { kept.hostDevice = hexKey(JSON.parse(row.certificate).hostCert?.device_public_key) || null; } catch { /* Not a certificate. */ }
    }
  }
  for (const [device, hash] of Object.entries(hashes)) namespaces[device].digest = hash.digest('hex');
  return namespaces;
}

/**
 * Whether a browser kept its state for a host set up again apart (`before`
 * and `after` from `keptFor`): the namespace it used for the old host holds
 * the old host's certificate and every row exactly as before, and the
 * profile's peer moved to a namespace named after, and certified by, another
 * host.
 */
export function keptApart(before, after) {
  const old = before?.namespaces?.[before.current];
  const oldNow = after?.namespaces?.[before?.current];
  const fresh = after?.namespaces?.[after.current];
  return Boolean(old?.rows && old.hostDevice && oldNow?.digest === old.digest && oldNow.hostDevice === old.hostDevice
    && after.current !== before.current && fresh?.hostDevice && fresh.hostDevice !== old.hostDevice
    && after.current === `${PROFILE_DEVICE}:${fresh.hostDevice}`);
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
export async function freePort(taken, span = 0) {
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
export async function startStack(options, ports, {fresh, env = {}}) {
  const args = [RUN_PY, '--scratch', options.scratch, '--bundle', options.bundle,
    '--relay-bin', options.relayBin, '--room-service-bin', options.roomServiceBin, '--name', 'auth',
    '--web-port', String(ports.web), '--api-port', String(ports.api), '--port-base', String(ports.base),
    ...(fresh ? ['--fresh'] : [])];
  // run.py hands TILO_ROOMS_* in its environment to the room service.
  const child = spawn('python3', args, {stdio: ['ignore', 'pipe', 'pipe'], env: {...process.env, ...env}});
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
export async function page(browser, context) {
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
    desktop: Boolean(globalThis.tiloMount),
    membership: (${membershipFor})(localStorage.getItem(${JSON.stringify(MEMBERSHIPS)}), ${JSON.stringify(relay)}),
    // Where a page that never settled stood: still loading its modules, or past them.
    loading: document.documentElement.dataset.entry ? undefined : {readyState: document.readyState,
      resources: performance.getEntriesByType('resource').length,
      unfinished: performance.getEntriesByType('resource').filter(r => !r.responseEnd).map(r => r.name).slice(0, 5)},
  }))()`);
}

// Every storage row a page of this origin keeps for `profile` (IndexedDB,
// storage-inline.mjs), with a digest of its bytes and the text of the
// profile's certificates; and the namespace each profile moved to.
const KEPT_ROWS = profile => `(async () => {
  const db = await new Promise((done, fail) => {
    const request = indexedDB.open('tilo-browser-records-v1');
    request.onupgradeneeded = () => request.transaction.abort();
    request.onsuccess = () => done(request.result);
    request.onerror = () => fail(request.error);
  });
  const hex = buffer => Array.from(new Uint8Array(buffer), byte => byte.toString(16).padStart(2, '0')).join('');
  const rows = [];
  try {
    const all = await new Promise((done, fail) => {
      const read = db.transaction('records', 'readonly').objectStore('records').getAll();
      read.onsuccess = () => done(read.result);
      read.onerror = () => fail(read.error);
    });
    for (const row of all) {
      if (typeof row?.namespace !== 'string' || typeof row.key !== 'string') continue;
      let namespace, key;
      try { namespace = JSON.parse(row.namespace); key = JSON.parse(row.key); } catch { continue; }
      if (namespace[0] !== ${JSON.stringify(profile)}) continue;
      const bytes = new Uint8Array(row.bytes ?? []);
      const certificate = key.package === 'so.tilo.browser-page' && key.record === 'certificate' && bytes.length
        ? new TextDecoder().decode(bytes) : null;
      rows.push({device: namespace[1], key: row.key, digest: hex(await crypto.subtle.digest('SHA-256', bytes)), certificate});
    }
  } finally { db.close(); }
  return {rows, devices: JSON.parse(localStorage.getItem(${JSON.stringify(PROFILE_DEVICES)}) ?? '{}')};
})()`;

/** What `context`'s browser keeps for `profile`, read by a page of the web
 * origin that does not open Tilo (its health check): `{namespaces, current}`.
 * A run that cannot read it fails the check that needs it. */
async function keptFor(browser, context, web, profile) {
  const probe = await browser.tab({context});
  try {
    await probe.navigate(`${web}/healthz`);
    await probe.waitFor("location.pathname === '/healthz' && document.readyState === 'complete'", {timeout: 30_000});
    const {rows, devices} = await probe.evaluate(KEPT_ROWS(profile));
    return {namespaces: keptNamespaces(rows), current: devices[profile] ?? PROFILE_DEVICE};
  } catch (error) {
    return {namespaces: {}, current: null, error: error.message};
  } finally {
    await probe.close();
  }
}

/** The prune horizon the pruned-membership check runs the host with, in
 * minutes, and how long it waits for the host to prune: the horizon, then
 * the next prune pass (the relay prunes at most once a minute, and a guest
 * it first sees on a pass is pruned two passes on), and a margin. */
export const PRUNE_AFTER_MINS = 1;
export const PRUNE_WAIT_MS = (2 * PRUNE_AFTER_MINS * 60 + 30) * 1000;

/** The expression a moderator's page evaluates to remove `person` from `room`
 * through the desktop's door: the room's own `remove-member` action,
 * confirmed. Resolves to `{ok, removed, error}`. */
export const REMOVE_MEMBER = (room, person) => `(async () => {
  try {
    const result = await globalThis.tiloMount.invokeAction(${JSON.stringify(`workspace.permissions:${room}`)}, 'remove-member',
      {person: ${JSON.stringify(person)}}, {confirm: true});
    const removed = result?.output?.value?.members?.find(member => member.userPublicKey === ${JSON.stringify(person)})?.removed ?? null;
    return {ok: result?.state === 'completed' && removed === true, state: result?.state ?? null, removed};
  } catch (error) {
    return {ok: false, error: String(error?.message ?? error)};
  }
})()`;

/** Remove `person` from `room` from a moderator's `tab`; resolves to what
 * the last attempt answered. The relay gives a declared moderator their role
 * once it governs the room, which may follow the room's start by a moment,
 * so a refusal is asked again a few times. */
async function removeMember(tab, room, person) {
  let answer = {ok: false, error: 'not asked'};
  for (let attempt = 1; attempt <= 5; attempt++) {
    answer = {...await tab.evaluate(REMOVE_MEMBER(room, person)).catch(error => ({ok: false, error: error.message})), attempt};
    if (answer.ok) break;
    await sleep(3000);
  }
  return answer;
}

/** Whether the page shows an entry card (an offer waiting for the person). */
const offering = tab => tab.evaluate("Boolean(document.querySelector('.tilo-entry-card'))").catch(() => false);

/** Marks the current document, so `settle` waits for the next one. */
const leaving = tab => tab.evaluate('globalThis.__authE2eLeft = true').catch(() => {});

/** Navigate `tab` to `url` (a fresh tab) and settle. */
export async function open(tab, url, settleOptions) {
  await tab.navigate(url);
  return settle(tab, settleOptions);
}

/** Reload `tab` and settle. */
export async function reload(tab, settleOptions) {
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

    // What the person's browser kept for the host before it is set up again.
    const profile = `tilo-identity:${me}@${host}`;
    const keptBefore = await keptFor(browser, person, web, profile);

    // A new root, relay and room behind the same origin.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: true});
    const changed = /set up again/;

    // stale-link: the old room's link after the reset.
    const stale = await page(browser, person);
    const staleOpened = await open(stale, roomLink(firstRoom), settleOptions);
    const staleOffered = await offering(stale);
    keepLog('stale-link', stale);
    check('stale-link', stack.room !== firstRoom && staleOpened.entry === 'refused' && !staleOffered
      && joinsTo(stale.requests, api).length === 0
      && staleOpened.membership?.userPublicKey === me && staleOpened.membership?.room === firstRoom,
    {entry: staleOpened.entry, offered: staleOffered, joins: joinsTo(stale.requests, api).length, status: staleOpened.status,
      membership: staleOpened.membership, room: stack.room});
    if (staleOpened.entry !== 'refused') await shoot('stale-link', stale);
    await stale.close();

    // host-declined: the bare entry offers to join the new host; not now.
    const declining = await page(browser, person);
    await declining.navigate(`${web}/`);
    const declineOffered = await press(declining, 'Not now', timeout).catch(() => false);
    const declined = await settle(declining, settleOptions);
    keepLog('host-declined', declining);
    check('host-declined', declineOffered && declined.entry === 'refused' && changed.test(declined.status)
      && joinsTo(declining.requests, api).length === 0 && declined.membership?.userPublicKey === me
      && declined.membership?.room === firstRoom,
    {offered: declineOffered, entry: declined.entry, joins: joinsTo(declining.requests, api).length,
      membership: declined.membership, status: declined.status, loading: declined.loading});
    if (declined.entry !== 'refused') await shoot('host-declined', declining);
    await declining.close();

    // host-reset: the same offer, accepted.
    const reset = await page(browser, person);
    await reset.navigate(`${web}/`);
    const resetOffered = await press(reset, 'Join again', timeout).catch(() => false);
    const rejoined = await settle(reset, settleOptions);
    keepLog('host-reset', reset);
    const resetJoins = joinsTo(reset.requests, api);
    check('host-reset', resetOffered && rejoined.entry === 'open' && rejoined.arrival === 'done'
      && resetJoins.length === 1 && resetJoins[0].status === 200
      && rejoined.membership?.userPublicKey === me && rejoined.membership?.room === stack.room
      && rejoined.hash.includes(`/w/${stack.room}`),
    {offered: resetOffered, entry: rejoined.entry, arrival: rejoined.arrival, joins: resetJoins.map(j => j.status),
      membership: rejoined.membership, room: stack.room, hash: rejoined.hash, status: rejoined.status, loading: rejoined.loading});
    if (rejoined.entry !== 'open') await shoot('host-reset', reset);

    // host-reload: once joined again, a reload makes no join.
    const beforeReload = reset.requests.length;
    const reloadedReset = await reload(reset, settleOptions);
    const reloadJoins = joinsTo(reset.requests.slice(beforeReload), api);
    check('host-reload', reloadedReset.entry === 'open' && reloadedReset.arrival === 'done' && reloadJoins.length === 0
      && reloadedReset.membership?.userPublicKey === me && reloadedReset.membership?.room === stack.room,
    {entry: reloadedReset.entry, arrival: reloadedReset.arrival, joins: reloadJoins.length, membership: reloadedReset.membership,
      status: reloadedReset.status});
    if (reloadedReset.entry !== 'open') await shoot('host-reload', reset);
    await reset.close();

    // kept-apart: the old host's state untouched, the new host's apart.
    const keptAfter = await keptFor(browser, person, web, profile);
    check('kept-apart', keptApart(keptBefore, keptAfter), {before: keptBefore, after: keptAfter});

    // stale-rejoined: the old room's link once joined again.
    const staleAgain = await page(browser, person);
    const staleAgainOpened = await open(staleAgain, roomLink(firstRoom), settleOptions);
    keepLog('stale-rejoined', staleAgain);
    check('stale-rejoined', staleAgainOpened.entry === 'refused' && joinsTo(staleAgain.requests, api).length === 0
      && staleAgainOpened.membership?.userPublicKey === me && staleAgainOpened.membership?.room === stack.room,
    {entry: staleAgainOpened.entry, joins: joinsTo(staleAgain.requests, api).length, status: staleAgainOpened.status,
      membership: staleAgainOpened.membership});
    if (staleAgainOpened.entry !== 'refused') await shoot('stale-rejoined', staleAgain);
    await staleAgain.close();

    // pruned-membership: the same host started again (its room kept) with a
    // short prune horizon, every tab of the person closed, and the person
    // away past it: the host prunes them.
    const keptRoom = stack.room;
    await stack.stop();
    stack = await startStack(options, ports, {fresh: false, env: {TILO_ROOMS_PRUNE_AFTER_MINS: String(PRUNE_AFTER_MINS)}});
    await sleep(PRUNE_WAIT_MS);
    const returning = await page(browser, person);
    const returned = await open(returning, `${web}/`, settleOptions);
    keepLog('pruned-membership', returning);
    const prunedJoins = joinsTo(returning.requests, api);
    check('pruned-membership', stack.room === keptRoom && returned.entry === 'open' && returned.arrival === 'done'
      && prunedJoins.length === 1 && prunedJoins[0].status === 200
      && returned.membership?.userPublicKey === me && returned.membership?.room === keptRoom,
    {entry: returned.entry, arrival: returned.arrival, joins: prunedJoins.map(j => j.status), membership: returned.membership,
      room: stack.room, keptRoom, status: returned.status, loading: returned.loading});
    if (returned.entry !== 'open') await shoot('pruned-membership', returning);
    await returning.close();

    // removed-member: the host started again with the person as one of the
    // room's moderators, and someone new in the room for them to remove.
    // This comes last: the removal cools the address every person here
    // joins from down for a day, and the room keeps that across restarts.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: false, env: {TILO_ROOMS_MODERATORS: me}});
    const moderator = await page(browser, person);
    const moderating = await open(moderator, `${web}/`, settleOptions);
    keepLog('moderator', moderator);
    if (moderating.entry !== 'open') await shoot('moderator', moderator);
    const visitor = await browser.context();
    const guest = await page(browser, visitor);
    const guestOpened = await open(guest, `${web}/`, settleOptions);
    const guestKey = guestOpened.membership?.userPublicKey ?? null;
    const guestJoins = joinsTo(guest.requests, api);
    if (guestOpened.entry !== 'open') await shoot('removed-guest', guest);
    const removal = moderating.entry === 'open' && /^[0-9a-f]{64}$/.test(guestKey ?? '') && guestKey !== me
      ? await removeMember(moderator, stack.room, guestKey) : {ok: false, error: 'no moderator and guest to remove'};
    // Their page, left open, makes no join of its own; reloaded, it is refused.
    const sinceRemoval = guest.requests.length;
    await sleep(10_000);
    const leftOpen = joinsTo(guest.requests.slice(sinceRemoval), api).length;
    const removedOpened = await reload(guest, settleOptions);
    keepLog('removed-member', guest);
    const removedJoins = joinsTo(guest.requests.slice(sinceRemoval), api);
    check('removed-member', moderating.entry === 'open' && moderating.membership?.userPublicKey === me
      && guestOpened.entry === 'open' && guestJoins.length === 1 && guestJoins[0].status === 200 && removal.ok
      && leftOpen === 0 && removedOpened.entry === 'refused' && !removedOpened.desktop && removedOpened.arrival !== 'done'
      && /\bremoved\b/.test(removedOpened.status) && removedJoins.length <= 1 && removedJoins.every(j => j.status === 403),
    {moderator: {entry: moderating.entry, membership: moderating.membership, status: moderating.status},
      guest: {entry: guestOpened.entry, phase: guestOpened.phase, arrival: guestOpened.arrival, joins: guestJoins.map(j => j.status),
        userPublicKey: guestKey, status: guestOpened.status, loading: guestOpened.loading}, removal, leftOpen,
      entry: removedOpened.entry, desktop: removedOpened.desktop, arrival: removedOpened.arrival,
      joins: removedJoins.map(j => j.status), membership: removedOpened.membership, status: removedOpened.status,
      hash: removedOpened.hash});
    if (removedOpened.entry !== 'refused') await shoot('removed-member', guest);
    await guest.close();

    // removed-entry: the bare entry, where the host's door is asked.
    const entering = await page(browser, visitor);
    const removedEntry = await open(entering, `${web}/`, settleOptions);
    keepLog('removed-entry', entering);
    const entryJoins = joinsTo(entering.requests, api);
    check('removed-entry', removedEntry.entry === 'refused' && !removedEntry.desktop && removedEntry.arrival !== 'done'
      && /\bremoved\b/.test(removedEntry.status) && /invitation/.test(removedEntry.status) && removedEntry.membership === null
      && entryJoins.length === 1 && entryJoins[0].status === 403,
    {entry: removedEntry.entry, desktop: removedEntry.desktop, arrival: removedEntry.arrival, joins: entryJoins.map(j => j.status),
      membership: removedEntry.membership, status: removedEntry.status});
    if (removedEntry.entry !== 'refused') await shoot('removed-entry', entering);
    await entering.close();

    // cooling-down: a fresh profile from the address the removed person
    // joined from.
    const newcomer = await page(browser, await browser.context());
    const turnedAway = await open(newcomer, `${web}/`, settleOptions);
    keepLog('cooling-down', newcomer);
    const coolingJoins = joinsTo(newcomer.requests, api);
    check('cooling-down', turnedAway.entry === 'refused' && !turnedAway.desktop && turnedAway.membership === null
      && coolingJoins.length === 1 && coolingJoins[0].status === 403 && /\bremoved\b/.test(turnedAway.status),
    {entry: turnedAway.entry, desktop: turnedAway.desktop, joins: coolingJoins.map(j => j.status),
      membership: turnedAway.membership, status: turnedAway.status});
    if (turnedAway.entry !== 'refused') await shoot('cooling-down', newcomer);
    await newcomer.close();
    await moderator.close();
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
