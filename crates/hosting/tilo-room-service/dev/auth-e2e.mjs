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
//                  the membership (host, room, person) and the room's link;
//                  it never asks to keep the site's storage (Firefox's
//                  prompt, stubbed as one nobody answers)
//   reload         the same tab reloaded: the desktop presents again as the
//                  same person in the same room, with no new join
//   second-tab     a second tab of that profile while the first is open: a
//                  tab is a device of its own, so it opens as a new device of
//                  the same person (one join at the door, which admits each
//                  device once), the membership kept as it was
//   tabs-sync      a message posted in Chat in one of the two tabs shows in
//                  the other
//   tabs-one-person
//                  another person in the room sees the two tabs as one
//                  person, and neither tab lists the other as someone else
//   visitor-types  that other person, a fresh visitor, opens Contacts, starts
//                  a new contact and types into its first name: the text
//                  lands there, not in another window
//   door-failing   a fresh profile whose join the door cannot answer (503
//                  without Retry-After, as when the room's relay does not
//                  answer in time) watches the room as its spectator: the
//                  desktop presents, through the spectator's own replica of
//                  the room, and nothing is kept as membership
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
//   revoked-device a guest's own root revokes the device of their open page
//                  (the relay's revocations, as `tilo-relay admin revoke`
//                  writes them): the page says this browser was removed from
//                  the person's devices, with one button, which opens their
//                  own desktop; it joins nothing and never asks to certify
//   forgotten-guest a guest's page stays open while the host is set up again
//                  (below): it joins the new host through its door by itself,
//                  once, as the same person under the same name, and the
//                  desktop presents, with no card asked of the person
//   host-reset     the host is set up again (run.py --fresh: a new root,
//                  relay and room) while the first person's browser keeps
//                  what it had for the old one; the bare entry joins the new
//                  host without asking: one join at the door, the desktop
//                  presents, and the membership names the new room as the
//                  same person
//   host-reload    that tab reloaded: presents again with no new join
//   kept-apart     what the browser kept for the old host is untouched (every
//                  row of its namespace as before the reset), and the new
//                  host's state lives in a namespace of its own, certified by
//                  the new host: the old state was never reused or handed to it
//   stale-rejoined the old room's link once joined again: refused without a
//                  join, the new membership kept
//   kept-membership
//                  the host started again (not anew), and the person away
//                  past the old prune passes: the host still has them. The bare entry joins at no door,
//                  and the desktop presents as the same person in the same
//                  room (membership persists; TILO_ROOMS_INACTIVE_AFTER_DAYS)
//   invite-door    the host started again taking people by invitation
//                  (TILO_ROOMS_ADMISSION=invite): a fresh profile opening the
//                  bare entry is refused at the door: a page of its own
//                  saying Tilo is invite-only, with the wait list's email
//                  field, signing in with an existing identity, and the
//                  invite link's field folded behind a toggle; no desktop
//                  and nothing kept (screenshots at 1440x900, 390x844 and
//                  2560x1440 at DPR 2); a pasted text that is no invite link
//                  is said so
//   wait-list      at that door, a text that is no email address is said so
//                  beside the field; an address is kept and confirmed as
//                  normalized, and the same address again is answered alike
//   wait-list-invite
//                  the first person, a member, opens "Waiting to join"
//                  through search and finds that address with when it was
//                  left (screenshots at 1440x900 and 390x844); Invite makes
//                  an invite link for it and Remove takes it off the list
//   invite-admits  the first person, a member, gets in as before, invites
//                  with the desktop's Invite; that link pasted into the door
//                  page's field asks to join, the member lets them in from
//                  the banner's button, pressed through accessibility, and
//                  the desktop presents as a member. Every check before ran
//                  with the door open (TILO_ROOMS_ADMISSION unset), as do the
//                  ones after
//   invite-admits-again
//                  three more strangers, each in a fresh browser, invited
//                  and let in the same way, each open as a member; one that
//                  is not keeps its console in the check's detail
//   removed-member the host started again with the person as one of the
//                  room's moderators (TILO_ROOMS_MODERATORS: a moderator
//                  removes the guests ranked below them); a second person
//                  joins, and the moderator removes them through the
//                  desktop's own door (the room's remove-member action,
//                  confirmed). The removed person's
//                  page, refused by the host, joins again through the door by
//                  itself (or, should the host not end its session, is
//                  reloaded): it is refused, saying they were removed, with
//                  no desktop, one button to their own desktop, and at most
//                  one join, which the door turns away
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

/** The membership `memberships` (localStorage's JSON) keeps for `origin`, or `null`;
 * `null` too without an origin (a caller that accepts any host, as the site web view). */
export function membershipFor(memberships, origin) {
  if (!origin) return null;
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
  // run.py hands TILO_ROOMS_* in its environment to the room service. Every
  // person here comes from 127.0.0.1, one address: the guest join limits
  // meant for a crowd's addresses (5 in ten minutes) turned the sixth
  // visitor of a run into a spectator.
  const joins = {TILO_ROOMS_GUEST_JOINS_PER_10_MIN: '100', TILO_ROOMS_GUEST_JOINS_PER_DAY: '1000'};
  const child = spawn('python3', args, {stdio: ['ignore', 'pipe', 'pipe'], env: {...process.env, ...joins, ...env}});
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
  // Firefox answers navigator.storage.persist() only once the person
  // dismisses its permission prompt; Chrome answers at once. Here it is a
  // prompt nobody answers, counted: getting in must neither ask nor wait.
  globalThis.__authE2ePersistAsked = 0;
  const storage = globalThis.navigator?.storage;
  if (storage) storage.persist = () => { globalThis.__authE2ePersistAsked += 1; return new Promise(() => {}); };
  const original = globalThis.fetch;
  const tell = entry => { try { globalThis.__authE2eFetch(JSON.stringify(entry)); } catch {} };
  globalThis.fetch = async function (input, init) {
    const url = new URL(input instanceof Request ? input.url : String(input), location.href).href;
    const method = String(init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
    try {
      const response = await original.call(this, input, init);
      const entry = {url, method, status: response.status};
      if (method === 'POST' && new URL(url).pathname.endsWith('/join')) {
        entry.body = await response.clone().text().catch(() => null);
        entry.request = typeof init?.body === 'string' ? init.body : null;
      }
      tell(entry);
      return response;
    } catch (error) {
      tell({url, method, error: String(error)});
      throw error;
    }
  };
})();`;

/** In the page: the door at `api` answers every join 503 without
 * Retry-After, as it does when the room's relay did not answer in time. */
export const doorFails = api => `(() => {
  const door = ${JSON.stringify(new URL('join', `${api}/`).href)};
  const original = globalThis.fetch;
  globalThis.fetch = function (input, init) {
    const url = new URL(input instanceof Request ? input.url : String(input), location.href).href;
    const method = String(init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
    if (method === 'POST' && url === door) {
      return Promise.resolve(new Response(JSON.stringify({error: 'the room did not answer in time'}),
        {status: 503, headers: {'content-type': 'application/json'}}));
    }
    return original.apply(this, arguments);
  };
})();`;

/** A page of `browser` in `context`, with the fetches it makes recorded;
 * the scripts `before` run in each document ahead of the recording. */
export async function page(browser, context, {before = []} = {}) {
  const tab = await browser.tab({context});
  const requests = [];
  const off = browser.on(message => {
    if (message.sessionId !== tab.sessionId || message.method !== 'Runtime.bindingCalled') return;
    if (message.params.name !== '__authE2eFetch') return;
    try { requests.push(JSON.parse(message.params.payload)); } catch { /* Not one of ours. */ }
  });
  await tab.send('Runtime.addBinding', {name: '__authE2eFetch'});
  for (const source of before) await tab.send('Page.addScriptToEvaluateOnNewDocument', {source});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FETCHES});
  await tab.send('Emulation.setFocusEmulationEnabled', {enabled: true});
  return {...tab, requests, close: async () => { off(); await tab.close(); }};
}

/** What the page shows once it settled: its entry state, status line, link
 * and the membership it keeps for the host at `relay`. */
async function settle(tab, {relay, timeout}) {
  // A page that never settles is that check's failure, reported with what
  // it showed by then; the run goes on to the next check.
  // The page keeps waiting for a graphics start that stopped moving on
  // (`data-gpu-stalled`: 60 s on one step without progress), offering Try
  // again; for this test that is a desktop that never drew, and the check
  // fails then, naming the step, instead of waiting out its whole timeout.
  const STALLED = "document.documentElement.hasAttribute('data-gpu-stalled') ? 'no first frame' : null";
  const state = await tab.waitFor(`(() => {
    const root = document.documentElement.dataset;
    if (globalThis.__authE2eLeft) return null;
    return ${JSON.stringify(SETTLED)}.includes(root.entry) ? root.entry : ${STALLED};
  })()`, {timeout}).catch(() => null);
  if (state === 'open') {
    // The desktop drew its first frame (`data-arrival` done); SwiftShader
    // compiles its pipelines on the first one.
    await tab.waitFor(`document.documentElement.dataset.arrival === 'done' || (${STALLED})`, {timeout}).catch(() => {});
  }
  return tab.evaluate(`(() => ({
    entry: globalThis.__authE2eLeft ? 'not loaded' : document.documentElement.dataset.entry ?? 'unsettled',
    phase: document.documentElement.dataset.phase ?? null,
    arrival: document.documentElement.dataset.arrival ?? null,
    status: document.getElementById('status')?.textContent ?? '',
    hash: location.hash,
    desktop: Boolean(globalThis.tiloMount),
    // A desktop that never drew: the graphics step it stopped on.
    firstFrame: document.documentElement.hasAttribute('data-gpu-stalled')
      ? \`none: stalled on \${document.documentElement.dataset.gpuStep ?? 'graphics start-up'}\` : undefined,
    persistAsked: globalThis.__authE2ePersistAsked ?? 0,
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

/** How long the kept-membership check keeps the person away from the
 * restarted host: past the hour-long horizon's old minute-long prune passes,
 * twice, and a margin. A person away this long, or an hour, is still a
 * member. */
export const AWAY_MS = (2 * 60 + 30) * 1000;
/** How long a removed person's open page is given to be refused by the host
 * and join again by itself before the check reloads it. */
export const REMOVED_REFUSED_MS = 4 * 60 * 1000;

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

const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/** BLAKE3 of one chunk (at most 1024 bytes), what the identity crate hashes
 * a revocation's preimage with: Node has no BLAKE3 of its own. */
export function blake3(input) {
  if (input.length > 1024) throw new RangeError('one chunk at most');
  const rotr = (x, n) => ((x >>> n) | (x << (32 - n))) >>> 0;
  const g = (s, a, b, c, d, x, y) => {
    s[a] = (s[a] + s[b] + x) >>> 0; s[d] = rotr(s[d] ^ s[a], 16);
    s[c] = (s[c] + s[d]) >>> 0; s[b] = rotr(s[b] ^ s[c], 12);
    s[a] = (s[a] + s[b] + y) >>> 0; s[d] = rotr(s[d] ^ s[a], 8);
    s[c] = (s[c] + s[d]) >>> 0; s[b] = rotr(s[b] ^ s[c], 7);
  };
  let cv = IV.slice();
  const blocks = Math.max(1, Math.ceil(input.length / 64));
  for (let index = 0; index < blocks; index++) {
    const block = new Uint8Array(64);
    const bytes = input.subarray(index * 64, index * 64 + 64);
    block.set(bytes);
    let m = Array.from({length: 16}, (_, i) => new DataView(block.buffer).getUint32(i * 4, true));
    const flags = (index === 0 ? 1 : 0) | (index === blocks - 1 ? 2 | 8 : 0);
    const s = [...cv, IV[0], IV[1], IV[2], IV[3], 0, 0, bytes.length, flags];
    for (let round = 0; round < 7; round++) {
      g(s, 0, 4, 8, 12, m[0], m[1]); g(s, 1, 5, 9, 13, m[2], m[3]);
      g(s, 2, 6, 10, 14, m[4], m[5]); g(s, 3, 7, 11, 15, m[6], m[7]);
      g(s, 0, 5, 10, 15, m[8], m[9]); g(s, 1, 6, 11, 12, m[10], m[11]);
      g(s, 2, 7, 8, 13, m[12], m[13]); g(s, 3, 4, 9, 14, m[14], m[15]);
      m = PERMUTATION.map(i => m[i]);
    }
    cv = cv.map((_, i) => (s[i] ^ s[i + 8]) >>> 0);
  }
  const out = new Uint8Array(32);
  cv.forEach((word, i) => new DataView(out.buffer).setUint32(i * 4, word, true));
  return out;
}

const unhex = text => Uint8Array.from(text.match(/../g) ?? [], byte => parseInt(byte, 16));
/** `RevocationReason::Stolen`, by its variant index. */
const STOLEN = 1;

/** What a root signs to revoke `device` (hex), its own device, since the
 * epoch's first nanosecond: `tilo_user_identity::revocation_signing_digest`,
 * BLAKE3 of the domain tag and the entry's postcard encoding. */
export function revocationDigest(user, device) {
  const tag = new TextEncoder().encode('tilo-revocation-v1\0');
  return blake3(Uint8Array.from([...tag, ...unhex(user), ...unhex(device), 1, STOLEN]));
}

/** The relay's `revocations.json` entry for that revocation, with the root's
 * `signature` (hex). */
export function revocationEntry(user, device, signature) {
  return {user_public_key: [...unhex(user)], revoked_device_public_key: [...unhex(device)], revoked_at_ns: 1,
    reason: 'Stolen', signature: [...unhex(signature)]};
}

/** The device key (hex) of the page's own certificate for `profile`, from
 * the namespace its peer is in now (`KEPT_ROWS`). */
export function currentDevice({rows, devices}, profile) {
  const current = devices[profile] ?? PROFILE_DEVICE;
  for (const row of rows) {
    if (row.device !== current || !row.certificate) continue;
    try { return hexKey(JSON.parse(row.certificate).cert.device_public_key); } catch { /* Not a certificate. */ }
  }
  return null;
}

/** The expression a page evaluates to sign `digest` (hex) with this
 * browser's person: their root, as `browserIdentity` keeps it. */
const SIGN_AS_PERSON = digest => `(async () => {
  const module = performance.getEntriesByType('resource').map(entry => entry.name).find(name => name.endsWith('/root-identity.mjs'));
  const {browserIdentity} = await import(module);
  const person = await browserIdentity({create: false});
  const bytes = Uint8Array.from(${JSON.stringify(digest)}.match(/../g), byte => parseInt(byte, 16));
  return Array.from(new Uint8Array(await person.sign(bytes)), byte => byte.toString(16).padStart(2, '0')).join('');
})()`;

/** What the page's refusal card shows: `{text, buttons}`, or `null`. */
const shownCard = tab => tab.evaluate(`(() => {
  const card = document.querySelector('.tilo-entry-card');
  return card ? {text: card.querySelector('p')?.textContent ?? '', buttons: [...card.querySelectorAll('button')].map(b => b.textContent)} : null;
})()`).catch(() => null);

/** `condition()` once truthy, polled every `every` ms; `null` after `timeout`. */
async function until(condition, timeout, every = 500) {
  const end = Date.now() + timeout;
  for (;;) {
    const value = await condition().catch(() => null);
    if (value) return value;
    if (Date.now() > end) return null;
    await sleep(every);
  }
}

/** In the page: the room's Chat window (opened if the desktop shows none),
 * the resource it posts on and the first text channel the room lists,
 * `{window, resource, channel}`. */
export const CHAT_CHANNEL = `(async () => {
  const chat = () => tiloMount.navigation.getSnapshot().entries.find(entry => entry.app === 'so.tilo.chat');
  if (!chat()) {
    const shown = tiloMount.navigation.getSnapshot();
    await tiloMount.navigation.dispatch({kind: 'open', app: 'so.tilo.chat', generation: shown.generation});
  }
  const window = chat()?.route.tab;
  if (!window) return null;
  const post = tiloMount.actions(undefined, {window}).find(action => action.address.action === 'post');
  if (!post) return null;
  // An app's action answers at once and a room's own a promise: either way
  // awaited, so a refusal reads as nothing rather than throwing.
  const room = await Promise.resolve().then(() => tiloMount.invokeAction('chat.workspace', 'read-room', {}, {window})).catch(() => null);
  const channel = room?.output?.value?.channels
    ?.find(({value}) => value.kind === 'text' && !value.archived && !value.conversation_with)?.id;
  return channel ? {window, resource: post.address.resource, channel} : null;
})()`;

/** In the page: why `CHAT_CHANNEL` finds nothing, step by step, for a
 * failure's report. */
export const CHAT_WHY = `(async () => {
  const shown = tiloMount.navigation.getSnapshot();
  const entry = shown.entries.find(entry => entry.app === 'so.tilo.chat');
  const window = entry?.route?.tab;
  if (!window) return {step: 'window', apps: shown.entries.map(entry => entry.app)};
  let actions;
  try {
    actions = tiloMount.actions(undefined, {window}).map(action => action.address.resource + ' ' + action.address.action);
  } catch (error) {
    return {step: 'actions', error: String(error?.message ?? error)};
  }
  if (!actions.some(action => action.endsWith(' post'))) return {step: 'post', actions: actions.slice(0, 40)};
  let room;
  try {
    room = await tiloMount.invokeAction('chat.workspace', 'read-room', {}, {window});
  } catch (error) {
    return {step: 'read-room', error: String(error?.message ?? error)};
  }
  const channels = room?.output?.value?.channels;
  return {step: 'channel', state: room?.state ?? null, error: room?.error ?? null,
    channels: Array.isArray(channels) ? channels.map(({id, value}) => ({id, kind: value?.kind, archived: value?.archived, with: value?.conversation_with})) : channels ?? null};
})()`;

/** Post `text` in Chat from `tab`: `{channel}` it went to, or `{error}`. */
async function postInChat(tab, text) {
  const chat = await until(() => tab.evaluate(CHAT_CHANNEL), 60_000);
  if (!chat) return {error: 'no Chat channel to post in', why: await tab.evaluate(CHAT_WHY).catch(error => error.message)};
  return tab.evaluate(`(async () => {
    const result = await tiloMount.invokeAction(${JSON.stringify(chat.resource)}, 'post',
      {channel: ${JSON.stringify(chat.channel)}, text: ${JSON.stringify(text)}}, {window: ${JSON.stringify(chat.window)}});
    return result?.state === 'completed' ? {channel: ${JSON.stringify(chat.channel)}} : {error: result?.state ?? 'not posted'};
  })()`).catch(error => ({error: error.message}));
}

/** In the page: the messages the Chat window `window` holds in the
 * channel with id `channel`, through the channel's own `read-messages`
 * (published on the channel's row, not the chat's root), as
 * `{messages}` or `{refused}`. An app's action answers at once and may
 * throw at once, so the call is made inside the try. */
export const READ_CHANNEL = (window, channel) => `(async () => {
  try {
    const result = await tiloMount.invokeAction(${JSON.stringify(`chat.workspace/channels/${channel}`)}, 'read-messages',
      {pageSize: 32}, {window: ${JSON.stringify(window)}});
    const messages = result?.output?.value?.messages;
    return Array.isArray(messages) ? {messages} : {refused: result?.error ?? result?.state ?? 'no messages'};
  } catch (error) {
    return {refused: String(error?.message ?? error)};
  }
})()`;

/** What `tab`'s Chat reads in the channel with id `channel`, for a
 * failure's detail: the last few texts, or why it read none. */
async function chatReads(tab, channel) {
  const chat = await tab.evaluate(CHAT_CHANNEL).catch(() => null);
  if (!chat) return {error: 'no Chat channel'};
  const read = await tab.evaluate(READ_CHANNEL(chat.window, channel)).catch(error => ({refused: error.message.slice(-300)}));
  return read.messages ? {count: read.messages.length, last: read.messages.slice(-3).map(m => m.text)} : read;
}

/** Whether `tab`'s Chat shows `text` in the channel with id `channel`. */
export async function chatShows(tab, channel, text) {
  const chat = await tab.evaluate(CHAT_CHANNEL);
  if (!chat) return false;
  const read = await tab.evaluate(READ_CHANNEL(chat.window, channel));
  return Boolean(read.messages?.some(message => message.text === text));
}

/** In the page: where the widget with test id `id` is on the page (CSS px,
 * its middle) and the value it reads, or null while the desktop shows none. */
const WIDGET = id => `(() => {
  const canvas = tiloMount.canvas.getBoundingClientRect();
  const find = node => {
    if (!node) return null;
    if (node.test_id === ${JSON.stringify(id)}) return node;
    for (const child of node.children ?? []) {
      const found = find(child);
      if (found) return found;
    }
    return null;
  };
  const node = find(tiloMount.state()?.semantics?.root);
  if (!node) return null;
  const [x, y, w, h] = node.rect;
  return {x: Math.round(canvas.left + x + w / 2), y: Math.round(canvas.top + y + h / 2),
    value: node.semantics?.value ?? null, focused: Boolean(node.semantics?.state?.focused)};
})()`;

/** In the page: press the control a reader knows as a button or menu item
 * named `name`, through assistive technology, as automation driving the
 * desktop by role does. Apps' toolbars are the window bar's slots, drawn by
 * the desktop as the bar's own children (or its More menu's items in a
 * narrow window), so they carry no test id of the app's. True once pressed,
 * null while the desktop shows no such control. */
const PRESS_NAMED = name => PRESS_WHERE(`name => name === ${JSON.stringify(name)}`);

/** In the page: the roles and names of what its desktop shows whose name
 * matches `pattern`, for a failure's detail. */
const NAMES_MATCHING = pattern => `(() => {
  const found = [];
  const named = name => new RegExp(${JSON.stringify(pattern)}).test(name ?? '');
  const walk = access => {
    for (const child of access?.children ?? []) {
      if (named(child.name)) found.push(child.role + ' ' + child.name);
      walk(child);
    }
  };
  const visit = node => {
    if (!node) return;
    if (named(node.semantics?.name)) found.push(node.semantics.role + ' ' + node.semantics.name);
    walk(node.semantics);
    for (const child of node.children ?? []) visit(child);
  };
  visit(tiloMount.state()?.semantics?.root);
  return found.slice(0, 20);
})()`;

/** In the page: press the first control whose name `named` (the source of
 * a function of the name) takes. */
const PRESS_WHERE = named => `(() => {
  const named = ${named};
  const pressable = new Set(['Button', 'MenuItem']);
  const find = node => {
    if (!node) return null;
    const own = node.semantics;
    if (own && pressable.has(own.role) && named(own.name)) {
      return {path: node.path, target: own.action_target ?? undefined};
    }
    const synthetic = (access, path) => {
      for (const child of access?.children ?? []) {
        if (pressable.has(child.role) && named(child.name) && child.action_target) {
          return {path, target: child.action_target};
        }
        const found = synthetic(child, path);
        if (found) return found;
      }
      return null;
    };
    const inside = synthetic(own, node.path);
    if (inside) return inside;
    for (const child of node.children ?? []) {
      const found = find(child);
      if (found) return found;
    }
    return null;
  };
  const control = find(tiloMount.state()?.semantics?.root);
  if (!control) return null;
  return tiloMount.semantic(control.path, control.target, 'activate', '', 0, 0) === true || null;
})()`;

/** In the page: the control whose name matches `pattern`, wherever the
 * desktop shows it (its own node or one of a node's synthetic children):
 * `{path, target, own, at}`, `at` its middle on the page where it has a
 * rect, or null while it shows none. */
const CONTROL_MATCHING = pattern => `(() => {
  const named = name => new RegExp(${JSON.stringify(pattern)}).test(name ?? '');
  const canvas = tiloMount.canvas.getBoundingClientRect();
  const middle = rect => rect && rect.length === 4
    ? {x: Math.round(canvas.left + rect[0] + rect[2] / 2), y: Math.round(canvas.top + rect[1] + rect[3] / 2)} : null;
  // A synthetic child's own bounds, where it publishes them ({x0, y0, x1, y1}).
  const within = bounds => bounds && Number.isFinite(bounds.x0)
    ? middle([bounds.x0, bounds.y0, bounds.x1 - bounds.x0, bounds.y1 - bounds.y0]) : null;
  const find = node => {
    if (!node) return null;
    const own = node.semantics;
    if (own && named(own.name)) return {path: node.path, target: own.action_target ?? null, own: true, at: middle(node.rect)};
    const synthetic = access => {
      for (const child of access?.children ?? []) {
        if (named(child.name)) {
          return {path: node.path, target: child.action_target ?? null, own: false, at: within(child.bounds) ?? middle(child.rect), bounds: child.bounds ?? null};
        }
        const found = synthetic(child);
        if (found) return found;
      }
      return null;
    };
    const inside = synthetic(own);
    if (inside) return inside;
    for (const child of node.children ?? []) {
      const found = find(child);
      if (found) return found;
    }
    return null;
  };
  return find(tiloMount.state()?.semantics?.root);
})()`;

/** Strangers invited and let in after the first, each in a fresh browser:
 * a failure that comes only sometimes shows in one of them. */
const INVITEE_ROUNDS = 3;

/** Press the control in `tab` whose name matches `pattern` through
 * assistive technology, as a screen reader does: by its own node, or by the
 * action target a synthetic child (a banner's button) publishes. Resolves
 * to `{control, pressed: 'semantic'}` once pressed, or the last attempt
 * with its `error`. */
async function pressMatching(tab, pattern, timeout) {
  let last = {pressed: false, error: 'not shown'};
  const done = await until(async () => {
    const control = await tab.evaluate(CONTROL_MATCHING(pattern));
    if (!control) return null;
    // Activating the owner of a synthetic child without a target of its
    // own would press something else: such a control is unreachable.
    if (!control.own && !control.target) {
      last = {control, pressed: false, error: 'no semantic target'};
      return null;
    }
    const pressed = await tab.evaluate(`tiloMount.semantic(${JSON.stringify(control.path)}, ${JSON.stringify(control.target ?? undefined)}, 'activate', '', 0, 0) === true`)
      .catch(error => ({error: error.message}));
    if (pressed === true) return (last = {control, pressed: 'semantic'});
    last = {control, pressed: false, error: pressed?.error ?? String(pressed)};
    return null;
  }, timeout, 2000);
  return done ?? last;
}

/** Click at `at` in `tab`, as a person's pointer does. */
async function click(tab, at) {
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x: at.x, y: at.y, button: 'left', clickCount: 1});
  }
}

/** Open Contacts in `tab`, start a new contact and type `text` into its
 * first name, key by key: `{value}`, what the field then reads, or
 * `{error}` naming the step that did not happen. */
async function typeInNewContact(tab, text) {
  await tab.evaluate(`(async () => {
    const shown = tiloMount.navigation.getSnapshot();
    await tiloMount.navigation.dispatch({kind: 'open', app: 'so.tilo.contacts', generation: shown.generation});
  })()`).catch(() => {});
  if (!await until(() => tab.evaluate(PRESS_NAMED('New contact')), 60_000)) {
    return {error: 'Contacts showed no New contact'};
  }
  const field = await until(() => tab.evaluate(WIDGET('contacts.form.first')), 30_000);
  if (!field) return {error: 'New contact showed no first name field'};
  // The form opens in place of the list and may still be settling where
  // it stands: click where the field is now until it takes focus.
  const focused = await until(async () => {
    const now = await tab.evaluate(WIDGET('contacts.form.first'));
    if (now?.focused) return now;
    if (now) await click(tab, now);
    return null;
  }, 20_000);
  if (!focused) {
    return {error: 'the first name field did not take focus'};
  }
  for (const key of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key, text: key});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key});
  }
  const landed = await until(async () => {
    const now = await tab.evaluate(WIDGET('contacts.form.first'));
    return now?.value === text ? now : null;
  }, 20_000);
  return landed ? {value: landed.value} : {value: (await tab.evaluate(WIDGET('contacts.form.first')))?.value ?? null};
}

/** Press `name` in `tab` as a keyboard does (CDP modifier bits: Ctrl 2). */
async function key(tab, name, modifiers = 0) {
  const named = {Enter: 13, Escape: 27};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const event = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...event, ...(name === 'Enter' ? {text: '\r'} : {})});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
  await sleep(300);
}

/** In `tab`, a member's desktop: open "Waiting to join" through search
 * (Ctrl+K, its name, Enter), find `contact` on the wait list, invite it
 * and take it off. `{row: {contact, added}, link, removed}`, or what did
 * not happen as `error`. */
async function inviteFromWaitList(tab, contact, shoot) {
  const card = "document.querySelector('[data-card=waiting]')";
  const ROW = `[...document.querySelectorAll('[data-card=waiting] [data-list=wait-list] li')]
    .find(li => li.querySelector('.tilo-waiting-contact')?.textContent === ${JSON.stringify(contact)})`;
  const opened = await until(async () => {
    if (await tab.evaluate(`Boolean(${card})`)) return true;
    await key(tab, 'k', 2);
    if (!await until(() => tab.evaluate('tiloMount.state()?.searchOpen === true || null'), 5000, 250)) return null;
    for (const ch of 'Waiting to join') {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    }
    await sleep(800);
    await key(tab, 'Enter');
    return await until(() => tab.evaluate(`Boolean(${card}) || null`), 5000, 250);
  }, 60_000, 1000);
  if (!opened) return {error: 'search did not open Waiting to join'};
  const row = await until(() => tab.evaluate(`(() => {
    const row = ${ROW};
    return row ? {contact: row.querySelector('.tilo-waiting-contact').textContent, added: row.querySelector('time')?.dateTime ?? null} : null;
  })()`), 30_000);
  if (!row) return {error: 'the address is not on the wait list', shown: await tab.evaluate(`${card}?.innerText ?? null`).catch(() => null)};
  for (const [name, width, height, mobile] of [['wait-list-inviter-1440x900', 1440, 900, false], ['wait-list-inviter-390x844', 390, 844, true]]) {
    await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 1, mobile});
    await sleep(500);
    await shoot(name, tab);
  }
  await tab.send('Emulation.clearDeviceMetricsOverride');
  await tab.evaluate(`[...${ROW}.querySelectorAll('button')].find(b => b.textContent === 'Invite').click()`);
  const link = await until(() => tab.evaluate("document.querySelector('[data-card=share] input[name=link]')?.value || null"), 30_000);
  await tab.evaluate("document.querySelector('[data-card=share] button[type=submit]')?.click()").catch(() => {});
  if (!link) return {row, error: 'Invite made no link'};
  await until(() => tab.evaluate(`Boolean(${ROW}) || null`), 10_000);
  await tab.evaluate(`[...${ROW}.querySelectorAll('button')].find(b => b.textContent === 'Remove').click()`).catch(() => {});
  const removed = Boolean(await until(() => tab.evaluate(`!${ROW} || null`), 30_000));
  await tab.evaluate(`${card}?.querySelector('button[type=submit]')?.click()`).catch(() => {});
  return {row, link, removed};
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
      && opened.membership?.room === firstRoom && /^[0-9a-f]{64}$/.test(me ?? '') && opened.hash.includes(`/w/${firstRoom}`)
      && opened.persistAsked === 0,
    {entry: opened.entry, arrival: opened.arrival, joins: joins.map(j => j.status), membership: opened.membership, hash: opened.hash,
      status: opened.status, loading: opened.loading, persistAsked: opened.persistAsked});
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

    // second-tab: the first still holds the profile's first device key, so
    // the second takes a key of its own (device-custody.mjs): a new device of
    // the same person, which the room admits through the door once.
    const second = await page(browser, person);
    const beside = await open(second, `${web}/`, settleOptions);
    keepLog('second-tab', second);
    newJoins = joinsTo(second.requests, api);
    check('second-tab', beside.entry === 'open' && beside.arrival === 'done' && newJoins.length === 1 && newJoins[0].status === 200
      && beside.membership?.userPublicKey === me && beside.membership?.room === firstRoom,
      {entry: beside.entry, arrival: beside.arrival, joins: newJoins.map(j => j.status), membership: beside.membership, status: beside.status});
    if (beside.entry !== 'open') await shoot('second-tab', second);

    // tabs-sync: what one tab posts, the other shows.
    const text = `From the first tab ${Date.now()}`;
    const posted = beside.entry === 'open' ? await postInChat(first, text) : {error: 'second tab not open'};
    const shown = posted.channel ? await until(() => chatShows(second, posted.channel, text), 60_000) : null;
    const reads = shown || !posted.channel ? undefined
      : {first: await chatReads(first, posted.channel), second: await chatReads(second, posted.channel)};
    check('tabs-sync', Boolean(posted.channel) && Boolean(shown), {posted, shown: Boolean(shown), reads});
    if (!shown) await shoot('tabs-sync', second);

    // tabs-one-person: someone else in the room sees one person, and each
    // tab sees only them.
    const other = await browser.context();
    const watcher = await page(browser, other);
    const watching = await open(watcher, `${web}/`, settleOptions);
    keepLog('tabs-one-person', watcher);
    const people = async tab => (await tab.evaluate('globalThis.tiloMount?.people?.() ?? null').catch(() => null))
      ?.filter(row => !row.agent) ?? null;
    const seen = async () => ({watcher: await people(watcher), first: await people(first), second: await people(second)});
    // Pages pair their devices with their person through the host's
    // `/here`, posted within 20 seconds of opening: until both tabs posted,
    // the watcher may still count two. Held for a few samples, so a moment
    // before the second tab is live does not pass.
    let together = null;
    if (watching.entry === 'open') {
      let held = 0;
      together = await until(async () => {
        const now = await seen();
        const ok = now.watcher?.length === 1 && now.first?.length === 1 && now.second?.length === 1
          && now.first[0].id === now.second[0].id && now.first[0].id !== now.watcher[0].id;
        held = ok ? held + 1 : 0;
        return held >= 3 ? now : null;
      }, 90_000, 2000);
    }
    check('tabs-one-person', Boolean(together), together ?? {entry: watching.entry, status: watching.status, seen: await seen()});
    if (!together) await shoot('tabs-one-person', watcher);

    // visitor-types: a fresh visitor's typing lands in an app other than
    // Chat, in a field they clicked, while the room's desktop goes on.
    const typed = watching.entry === 'open' ? await typeInNewContact(watcher, 'Ada') : {error: 'the visitor is not in'};
    check('visitor-types', typed.value === 'Ada', typed);
    if (typed.value !== 'Ada') await shoot('visitor-types', watcher);
    await watcher.close();

    // door-failing: the door cannot answer a fresh visitor's join, so the
    // page watches the room as its spectator. A spectator's device lives
    // only in memory; its desktop still needs the room replicated, which
    // once failed every such visit with "the peer has no replicated room".
    const passerby = await page(browser, await browser.context(), {before: [doorFails(api)]});
    const watched = await open(passerby, `${web}/`, settleOptions);
    keepLog('door-failing', passerby);
    const turned = joinsTo(passerby.requests, api);
    const spectating = passerby.requests.filter(request => request.method === 'POST'
      && request.url === new URL('spectate', `${api}/`).href);
    check('door-failing', watched.entry === 'open' && watched.arrival === 'done' && watched.desktop
      && turned.length === 1 && turned[0].status === 503 && spectating.length === 1 && spectating[0].status === 200
      && watched.membership === null,
    {entry: watched.entry, arrival: watched.arrival, joins: turned.map(j => j.status), spectate: spectating.map(s => s.status),
      membership: watched.membership, status: watched.status, firstFrame: watched.firstFrame});
    if (watched.arrival !== 'done') await shoot('door-failing', passerby);
    await passerby.close();

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

    // revoked-device: a guest's own root revokes the device of their open page.
    const revoker = await browser.context();
    const revokedTab = await page(browser, revoker);
    const revokedOpened = await open(revokedTab, `${web}/`, settleOptions);
    const revokedPerson = revokedOpened.membership?.userPublicKey ?? null;
    let revocation = {written: false};
    if (revokedOpened.entry === 'open' && revokedPerson) {
      try {
        const revokedProfile = `tilo-identity:${revokedPerson}@${host}`;
        const device = currentDevice(await revokedTab.evaluate(KEPT_ROWS(revokedProfile)), revokedProfile);
        const digest = Buffer.from(revocationDigest(revokedPerson, device)).toString('hex');
        const signature = await revokedTab.evaluate(SIGN_AS_PERSON(digest));
        // The room's relay (its state root under the room's relay directory)
        // reads its revocations again when the file changes.
        writeFileSync(join(options.scratch, 'state/rooms/auth/relay/relay/revocations.json'),
          `${JSON.stringify([revocationEntry(revokedPerson, device, signature)], null, 2)}\n`);
        revocation = {written: true, device};
      } catch (error) {
        revocation = {written: false, error: error.message};
      }
    }
    const sinceRevocation = revokedTab.requests.length;
    const refusal = revocation.written
      ? await revokedTab.waitFor('document.documentElement.dataset.refusal ?? null', {timeout}).catch(() => null) : null;
    const revokedCard = await shownCard(revokedTab);
    await shoot('revoked-device', revokedTab);
    keepLog('revoked-device', revokedTab);
    await leaving(revokedTab);
    const ownPressed = revokedCard ? await press(revokedTab, 'Open your own desktop', timeout).catch(() => false) : false;
    const ownDesktop = ownPressed ? await settle(revokedTab, settleOptions) : null;
    check('revoked-device', revocation.written && refusal === 'device-revoked'
      && /removed from your devices/.test(revokedCard?.text ?? '') && !/certif/i.test(revokedCard?.text ?? '')
      && revokedCard?.buttons?.length === 1 && joinsTo(revokedTab.requests.slice(sinceRevocation), api).length === 0
      && ownDesktop?.entry === 'open' && ownDesktop.hash.startsWith('#/me'),
    {opened: revokedOpened.entry, revocation, refusal, card: revokedCard,
      joins: joinsTo(revokedTab.requests.slice(sinceRevocation), api).length, own: ownDesktop && {entry: ownDesktop.entry, hash: ownDesktop.hash}});
    await revokedTab.close();

    // A guest whose page stays open while the host is set up again below.
    const forgetting = await browser.context();
    const forgotten = await page(browser, forgetting);
    const forgottenOpened = await open(forgotten, `${web}/`, settleOptions);
    const nameOf = join => { try { return JSON.parse(join?.request ?? 'null')?.displayName ?? null; } catch { return null; } };
    const forgottenName = nameOf(joinsTo(forgotten.requests, api)[0]);

    // What the person's browser kept for the host before it is set up again.
    const profile = `tilo-identity:${me}@${host}`;
    const keptBefore = await keptFor(browser, person, web, profile);

    // A new root, relay and room behind the same origin.
    await leaving(forgotten);
    const sinceReset = forgotten.requests.length;
    await stack.stop();
    stack = await startStack(options, ports, {fresh: true});
    // forgotten-guest: the open page joins the new host by itself.
    const connecting = await forgotten.waitFor(`(() => {
      const arrival = document.documentElement.dataset.arrival;
      return !globalThis.__authE2eLeft && arrival && arrival !== 'done' ? arrival : null;
    })()`, {timeout, interval: 50}).catch(() => null);
    if (connecting) await shoot('forgotten-guest-connecting', forgotten);
    const forgottenBack = await settle(forgotten, settleOptions);
    const forgottenAsked = await offering(forgotten);
    await shoot('forgotten-guest-rejoined', forgotten);
    keepLog('forgotten-guest', forgotten);
    const rejoins = joinsTo(forgotten.requests.slice(sinceReset), api);
    check('forgotten-guest', forgottenOpened.entry === 'open' && forgottenBack.entry === 'open'
      && forgottenBack.arrival === 'done' && rejoins.length === 1 && rejoins[0].status === 200 && !forgottenAsked
      && /^[0-9a-f]{64}$/.test(forgottenOpened.membership?.userPublicKey ?? '')
      && forgottenBack.membership?.userPublicKey === forgottenOpened.membership.userPublicKey
      && forgottenBack.membership?.room === stack.room && Boolean(forgottenName) && nameOf(rejoins[0]) === forgottenName,
    {opened: forgottenOpened.entry, connecting, entry: forgottenBack.entry, arrival: forgottenBack.arrival,
      joins: rejoins.map(j => j.status), asked: forgottenAsked, before: forgottenOpened.membership, after: forgottenBack.membership,
      room: stack.room, names: [forgottenName, nameOf(rejoins[0])], status: forgottenBack.status});
    await forgotten.close();


    // host-reset: the bare entry joins the new host, asking nothing.
    const reset = await page(browser, person);
    await reset.navigate(`${web}/`);
    const rejoined = await settle(reset, settleOptions);
    const resetOffered = await offering(reset);
    keepLog('host-reset', reset);
    const resetJoins = joinsTo(reset.requests, api);
    check('host-reset', !resetOffered && rejoined.entry === 'open' && rejoined.arrival === 'done'
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

    // kept-membership: the same host started again (its room kept), every
    // tab of the person closed, and the person away past its prune passes:
    // they are still a member, and come back without joining again.
    const keptRoom = stack.room;
    await stack.stop();
    stack = await startStack(options, ports, {fresh: false});
    await sleep(AWAY_MS);
    const returning = await page(browser, person);
    const returned = await open(returning, `${web}/`, settleOptions);
    keepLog('kept-membership', returning);
    const keptJoins = joinsTo(returning.requests, api);
    check('kept-membership', stack.room === keptRoom && returned.entry === 'open' && returned.arrival === 'done'
      && keptJoins.length === 0
      && returned.membership?.userPublicKey === me && returned.membership?.room === keptRoom,
    {entry: returned.entry, arrival: returned.arrival, joins: keptJoins.map(j => j.status), membership: returned.membership,
      room: stack.room, keptRoom, status: returned.status, loading: returned.loading});
    if (returned.entry !== 'open') await shoot('kept-membership', returning);
    await returning.close();

    // invite-door: the host started again taking people by invitation.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: false, env: {TILO_ROOMS_ADMISSION: 'invite'}});
    const outsider = await browser.context();
    const door = await page(browser, outsider);
    // Until its relay has the room back, the door answers 503 and the page
    // watches live; asked again once it is back, it turns them away.
    let doorTurned = await open(door, `${web}/`, settleOptions);
    if (doorTurned.entry !== 'refused') {
      doorTurned = await until(async () => {
        const now = await reload(door, settleOptions);
        return now.entry === 'refused' ? now : null;
      }, 90_000, 3000) ?? doorTurned;
    }
    keepLog('invite-door', door);
    const doorJoins = joinsTo(door.requests, api);
    // The door: shown, apologising that Tilo is invite-only, the wait list's
    // field first, signing in and the invite link's toggle below it, the
    // link's field folded away until asked for.
    const DOOR = `(() => {
      const shown = id => { const el = document.getElementById(id); return Boolean(el) && getComputedStyle(el).display !== 'none' && el.getBoundingClientRect().height > 0; };
      return {door: shown('arrival-door'), waitList: shown('arrival-wait'), signIn: shown('arrival-sign-in'), toggle: shown('arrival-invite-toggle'),
        link: shown('arrival-invite'), inDoor: document.getElementById('arrival-invite')?.closest('#arrival-door') !== null,
        heading: document.getElementById('arrival-door-heading')?.textContent ?? '', note: document.getElementById('arrival-wait-note')?.textContent ?? '',
        done: shown('arrival-wait-done') ? document.getElementById('arrival-wait-done').textContent : null,
        invalid: document.getElementById('arrival-wait-email')?.getAttribute('aria-invalid') === 'true'};
    })()`;
    const field = await door.evaluate(DOOR).catch(error => ({error: error.message}));
    for (const [name, width, height, mobile, deviceScaleFactor = 1] of [['invite-door-1440x900', 1440, 900, false],
      ['invite-door-390x844', 390, 844, true], ['invite-door-2560x1440-dpr2', 2560, 1440, false, 2]]) {
      await door.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor, mobile});
      await sleep(500);
      await shoot(name, door);
    }
    await door.send('Emulation.clearDeviceMetricsOverride');
    const notALink = await door.evaluate(`(() => {
      document.getElementById('arrival-invite-toggle').click();
      document.getElementById('arrival-invite-link').value = 'hello';
      document.getElementById('arrival-invite').requestSubmit();
      return document.getElementById('arrival-invite-error').textContent;
    })()`).catch(error => `failed: ${error.message}`);
    const unfolded = await door.evaluate(DOOR).catch(error => ({error: error.message}));
    check('invite-door', doorTurned.entry === 'refused' && !doorTurned.desktop && doorTurned.membership === null
      && /need an invitation/.test(doorTurned.status) && field.door && field.waitList && field.signIn && field.toggle && !field.link
      && /invite-only/.test(field.heading) && /only use your email/.test(field.note) && unfolded.link && unfolded.inDoor
      && doorJoins.at(-1)?.status === 403 && /invitation-only/.test(doorJoins.at(-1)?.body ?? '')
      && /"waitList":true/.test(doorJoins.at(-1)?.body ?? '') && /isn’t an invite link/.test(notALink),
    {entry: doorTurned.entry, desktop: doorTurned.desktop, membership: doorTurned.membership, status: doorTurned.status, field, unfolded,
      joins: doorJoins.map(j => ({status: j.status, body: j.body})), notALink});

    // wait-list: an address that is none is said so on the page, a real one
    // is kept and confirmed, and the same one again (as typed differently)
    // is answered the same without a second entry.
    const leave = text => door.evaluate(`(async () => {
      const email = document.getElementById('arrival-wait-email');
      email.value = ${JSON.stringify(text)};
      email.dispatchEvent(new Event('input'));
      document.getElementById('arrival-wait').requestSubmit();
      const deadline = Date.now() + 15000;
      while (Date.now() < deadline && document.querySelector('#arrival-wait button')?.disabled !== false) await new Promise(r => setTimeout(r, 100));
      await new Promise(r => setTimeout(r, 300));
      return ${DOOR};
    })()`).catch(error => ({error: error.message}));
    const badAddress = await leave('not an address');
    await shoot('wait-list-invalid', door).catch(() => {});
    const waitListed = await leave('Waiting.Person@Example.org ');
    await shoot('wait-list-done', door).catch(() => {});
    const listedAgain = await fetch(`${api}/wait-list`, {method: 'POST', headers: {'content-type': 'application/json'},
      body: JSON.stringify({email: 'waiting.person@example.org'})}).then(async r => ({status: r.status, body: await r.json().catch(() => null)}))
      .catch(error => ({error: error.message}));
    check('wait-list', badAddress.invalid && /doesn’t look like an email/.test(badAddress.note) && badAddress.done === null
      && /on the list.+waiting\.person@example\.org/.test(waitListed.done ?? '') && listedAgain.status === 200
      && listedAgain.body?.email === 'waiting.person@example.org', {badAddress, waitListed, listedAgain});

    // invite-admits: the first person invites the stranger.
    const inviter = await page(browser, person);
    const inviting = await open(inviter, `${web}/`, settleOptions);
    keepLog('inviter', inviter);
    // wait-list-invite: the member finds the address in "Waiting to join"
    // (through search, as a person does), with when it was left, invites it
    // with a link of its own and takes it off the list.
    const listed = inviting.entry === 'open' ? await inviteFromWaitList(inviter, 'waiting.person@example.org', shoot)
      : {error: 'the inviter is not in'};
    check('wait-list-invite', listed.row?.contact === 'waiting.person@example.org' && Boolean(listed.row?.added)
      && /[?&]invite=/.test(listed.link ?? '') && listed.removed === true, listed);
    const invitePressed = inviting.entry === 'open' && Boolean(await until(() => inviter.evaluate(PRESS_NAMED('Invite')), 60_000));
    const inviteLink = invitePressed ? await until(() => inviter.evaluate(
      "document.querySelector('[data-card=share] input[name=link]')?.value || null"), 30_000) : null;
    await inviter.evaluate("document.querySelector('[data-card=share] button[type=submit]')?.click()").catch(() => {});
    const sinceInvite = door.requests.length;
    let invited = {entry: 'not asked'};
    let joined = false;
    let letIn = {pressed: false, error: 'no invitation'};
    if (inviteLink) {
      await leaving(door);
      await door.evaluate(`(() => {
        document.getElementById('arrival-invite-link').value = ${JSON.stringify(` ${inviteLink} `)};
        document.getElementById('arrival-invite').requestSubmit();
      })()`).catch(() => {});
      await door.waitFor('!globalThis.__authE2eLeft', {timeout}).catch(() => {});
      joined = await press(door, 'Join', timeout);
      // Let in from the banner the inviter's desktop offers; once the
      // invitee is in, nothing more is offered.
      letIn = await pressMatching(inviter, '^Let .+ in$', 90_000);
      invited = await until(async () => {
        const now = await settle(door, settleOptions);
        return now.entry === 'open' && now.membership ? now : null;
      }, 90_000) ?? await settle(door, settleOptions);
    }
    keepLog('invite-admits', door);
    const invitedJoins = joinsTo(door.requests.slice(sinceInvite), api);
    const asking = letIn.pressed ? null : await inviter.evaluate(NAMES_MATCHING('asks to join|^Let |Waiting')).catch(error => error.message);
    check('invite-admits', inviting.entry === 'open' && inviting.membership?.userPublicKey === me && Boolean(inviteLink)
      && /[?&]invite=/.test(inviteLink ?? '') && joined && Boolean(letIn.pressed) && invited.entry === 'open' && invited.arrival === 'done'
      && /^[0-9a-f]{64}$/.test(invited.membership?.userPublicKey ?? '') && invited.membership.userPublicKey !== me
      && invitedJoins.length === 0,
    {inviter: {entry: inviting.entry, membership: inviting.membership, status: inviting.status}, invitePressed, inviteLink,
      joined, letIn, asking, inviterLog: letIn.pressed ? undefined : inviter.log.slice(-8),
      inviteeLog: invited.entry === 'open' ? undefined : door.log.slice(-12), entry: invited.entry, arrival: invited.arrival, membership: invited.membership, status: invited.status,
      firstFrame: invited.firstFrame, doorJoins: invitedJoins.map(j => j.status)});
    if (invited.entry !== 'open') await shoot('invite-admits', door);
    await door.close();

    // invite-admits-again: more strangers, each in a browser of their own,
    // invited and let in the same way. Being let in opens the workspace
    // every time, not most times; one that does not keeps its console.
    const rounds = [];
    for (let round = 1; round <= INVITEE_ROUNDS && inviting.entry === 'open'; round++) {
      const pressedInvite = Boolean(await until(() => inviter.evaluate(PRESS_NAMED('Invite')), 60_000));
      const link = pressedInvite ? await until(() => inviter.evaluate(
        "document.querySelector('[data-card=share] input[name=link]')?.value || null"), 30_000) : null;
      await inviter.evaluate("document.querySelector('[data-card=share] button[type=submit]')?.click()").catch(() => {});
      if (!link) {
        rounds.push({round, error: 'no invite link', pressedInvite});
        continue;
      }
      const stranger = await page(browser, await browser.context());
      await stranger.navigate(link);
      const joinedAgain = await press(stranger, 'Join', timeout);
      // Each step of getting in and drawing, timed from being let in: how
      // long the first frame takes tells a slow software rasteriser from a
      // frame that never comes.
      await stranger.evaluate(`(() => {
        const steps = globalThis.__authE2eSteps = [];
        const note = () => {
          const {arrival, phase, gpuStep, frames} = document.documentElement.dataset;
          const stalled = document.documentElement.hasAttribute('data-gpu-stalled');
          const last = steps.at(-1);
          if (!last || last.arrival !== arrival || last.phase !== phase || last.gpuStep !== gpuStep || last.stalled !== stalled || (last.frames === '0') !== (frames === '0')) {
            steps.push({at: Math.round(performance.now()), arrival, phase, gpuStep, frames, stalled});
          }
        };
        note();
        new MutationObserver(note).observe(document.documentElement, {attributes: true});
      })()`).catch(() => {});
      const pressed = await pressMatching(inviter, '^Let .+ in$', 90_000);
      const letInAt = await stranger.evaluate('Math.round(performance.now())').catch(() => null);
      const now = await until(async () => {
        const state = await settle(stranger, settleOptions);
        return state.entry === 'open' && state.membership ? state : null;
      }, 90_000) ?? await settle(stranger, settleOptions);
      const opened = now.entry === 'open' && now.arrival === 'done';
      const steps = await stranger.evaluate('globalThis.__authE2eSteps ?? []').catch(() => []);
      rounds.push({round, joined: joinedAgain, letIn: pressed.pressed || pressed.error, entry: now.entry, arrival: now.arrival,
        status: now.status, firstFrame: now.firstFrame, steps: steps.map(step => ({...step, at: letInAt === null ? step.at : step.at - letInAt})),
        console: opened ? undefined : stranger.log.slice(-40)});
      if (!opened) {
        keepLog(`invite-admits-again-${round}`, stranger);
        await shoot(`invite-admits-again-${round}`, stranger);
      }
      await stranger.close();
    }
    check('invite-admits-again', rounds.length === INVITEE_ROUNDS
      && rounds.every(r => r.joined && r.letIn === 'semantic' && r.entry === 'open' && r.arrival === 'done'), {rounds});
    await inviter.close();

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
    let guestOpened = await open(guest, `${web}/`, settleOptions);
    // The host just started again: until its relay has the room back, the
    // door answers 503 and the page watches live, then joins by itself.
    // What is checked here is the removal, so wait for that join.
    if (!guestOpened.membership) {
      guestOpened = await until(async () => {
        const now = await settle(guest, settleOptions);
        return now.membership ? now : null;
      }, 90_000) ?? guestOpened;
    }
    const guestKey = guestOpened.membership?.userPublicKey ?? null;
    const guestJoins = joinsTo(guest.requests, api);
    if (guestOpened.entry !== 'open') await shoot('removed-guest', guest);
    const removal = moderating.entry === 'open' && /^[0-9a-f]{64}$/.test(guestKey ?? '') && guestKey !== me
      ? await removeMember(moderator, stack.room, guestKey) : {ok: false, error: 'no moderator and guest to remove'};
    // Their page, left open, is refused by the host once it narrows whom it
    // admits, and joins again through the door by itself, which turns it
    // away; one the host has not refused by then is reloaded instead (and
    // may have joined meanwhile).
    const sinceRemoval = guest.requests.length;
    await leaving(guest);
    let removedOpened = await settle(guest, {...settleOptions, timeout: REMOVED_REFUSED_MS});
    const byItself = removedOpened.entry !== 'not loaded';
    if (!byItself) removedOpened = await reload(guest, settleOptions);
    const removedOwn = await guest.evaluate("getComputedStyle(document.getElementById('arrival-own')).display !== 'none'").catch(() => false);
    await shoot('removed-member', guest);
    keepLog('removed-member', guest);
    const removedJoins = joinsTo(guest.requests.slice(sinceRemoval), api);
    check('removed-member', moderating.entry === 'open' && moderating.membership?.userPublicKey === me
      && guestOpened.entry === 'open' && guestJoins.at(-1)?.status === 200
      && guestJoins.slice(0, -1).every(j => j.status === 503) && removal.ok
      && removedOpened.entry === 'refused' && !removedOpened.desktop && removedOpened.arrival !== 'done' && removedOwn
      && /\bremoved\b/.test(removedOpened.status) && removedJoins.length <= (byItself ? 1 : 2)
      && removedJoins.every(j => j.status === 403),
    {moderator: {entry: moderating.entry, membership: moderating.membership, status: moderating.status},
      guest: {entry: guestOpened.entry, phase: guestOpened.phase, arrival: guestOpened.arrival, joins: guestJoins.map(j => j.status === 200 ? 200 : {status: j.status, body: j.body}),
        userPublicKey: guestKey, status: guestOpened.status, loading: guestOpened.loading}, removal, byItself, ownDesktop: removedOwn,
      entry: removedOpened.entry, desktop: removedOpened.desktop, arrival: removedOpened.arrival,
      joins: removedJoins.map(j => j.status), membership: removedOpened.membership, status: removedOpened.status,
      hash: removedOpened.hash});
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
