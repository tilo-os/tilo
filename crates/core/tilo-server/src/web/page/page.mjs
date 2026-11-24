// A workspace's app as a web page, for a browser that is already a device of
// one of the workspace's members (it signed in to Tilo here before).
//
// The page holds no app: the server renders the app's view as HTML. This
// script proves the browser's device to the server on a WebTransport session,
// with the handshake every device's session opens with, asks for the page,
// and sends back each click and edit, one at a time, applying the elements
// that changed. Typing shows at once in the field itself; the server's answer
// never moves the caret: a field being typed in keeps its own text until the
// person stops, and is only then set to the server's, if that differs.

const CLIENT_TAG = new TextEncoder().encode('tilo-server-auth-v2/client\0');
const MAX_FRAME = 8 * 1024 * 1024;

// ---- BLAKE3 (the handshake's digests and an act's signed call) ----

const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const PERMUTE = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const CHUNK_START = 1, CHUNK_END = 2, PARENT = 4, ROOT = 8;

/** BLAKE3's compression of one 64-byte `block` (as 16 words): the first eight words of what it
 * outputs, the chaining value. */
function compress(cv, block, counter, length, flags) {
  let m = block;
  const v = [...cv, IV[0], IV[1], IV[2], IV[3], counter >>> 0, Math.floor(counter / 2 ** 32), length, flags];
  const rotr = (x, n) => ((x >>> n) | (x << (32 - n))) >>> 0;
  const g = (a, b, c, d, x, y) => {
    v[a] = (v[a] + v[b] + x) >>> 0; v[d] = rotr(v[d] ^ v[a], 16);
    v[c] = (v[c] + v[d]) >>> 0; v[b] = rotr(v[b] ^ v[c], 12);
    v[a] = (v[a] + v[b] + y) >>> 0; v[d] = rotr(v[d] ^ v[a], 8);
    v[c] = (v[c] + v[d]) >>> 0; v[b] = rotr(v[b] ^ v[c], 7);
  };
  for (let round = 0; round < 7; round++) {
    g(0, 4, 8, 12, m[0], m[1]); g(1, 5, 9, 13, m[2], m[3]);
    g(2, 6, 10, 14, m[4], m[5]); g(3, 7, 11, 15, m[6], m[7]);
    g(0, 5, 10, 15, m[8], m[9]); g(1, 6, 11, 12, m[10], m[11]);
    g(2, 7, 8, 13, m[12], m[13]); g(3, 4, 9, 14, m[14], m[15]);
    m = PERMUTE.map(i => m[i]);
  }
  return Array.from({length: 8}, (_, i) => (v[i] ^ v[i + 8]) >>> 0);
}

const words = (bytes, at, length) => {
  const block = new Uint8Array(64);
  block.set(bytes.subarray(at, at + length));
  const view = new DataView(block.buffer);
  return Array.from({length: 16}, (_, i) => view.getUint32(i * 4, true));
};

/** BLAKE3 of `input`. */
export function blake3(input) {
  // What a node of the tree outputs, compressed last: as a chaining value, or with ROOT, the hash.
  const output = (cv, block, counter, length, flags) => extra => compress(cv, block, counter, length, flags | extra);
  const chunk = index => {
    const start = index * 1024;
    const length = Math.min(1024, input.length - start);
    const blocks = Math.max(1, Math.ceil(length / 64));
    let cv = IV;
    for (let b = 0; b < blocks - 1; b++) cv = compress(cv, words(input, start + b * 64, 64), index, 64, b === 0 ? CHUNK_START : 0);
    const last = length - (blocks - 1) * 64;
    return output(cv, words(input, start + (blocks - 1) * 64, last), index, last, (blocks === 1 ? CHUNK_START : 0) | CHUNK_END);
  };
  // A subtree's left half holds the largest power of two of its chunks that leaves the right one.
  const tree = (first, count) => {
    if (count === 1) return chunk(first);
    const left = 2 ** Math.floor(Math.log2(count - 1));
    return output(IV, [...tree(first, left)(0), ...tree(first + left, count - left)(0)], 0, 64, PARENT);
  };
  const root = tree(0, Math.max(1, Math.ceil(input.length / 1024)))(ROOT);
  const out = new Uint8Array(32);
  const view = new DataView(out.buffer);
  root.forEach((word, i) => view.setUint32(i * 4, word, true));
  return out;
}

// ---- The certificate, read exactly ----

/** `text` as JSON, its integers past what a number holds exactly as BigInt:
 * a certificate's nanosecond times are signed, so a rounded one is another. */
export function parseExact(text) {
  let at = 0;
  const blank = () => { while (/\s/.test(text[at])) at++; };
  const value = () => {
    blank();
    const c = text[at];
    if (c === '{') {
      at++; const object = {}; blank();
      if (text[at] === '}') { at++; return object; }
      for (;;) {
        blank(); const key = value(); blank(); at++; // ':'
        object[key] = value(); blank();
        if (text[at++] === '}') return object;
      }
    }
    if (c === '[') {
      at++; const list = []; blank();
      if (text[at] === ']') { at++; return list; }
      for (;;) { list.push(value()); blank(); if (text[at++] === ']') return list; }
    }
    if (c === '"') {
      const start = at; at++;
      while (text[at] !== '"') at += text[at] === '\\' ? 2 : 1;
      at++;
      return JSON.parse(text.slice(start, at));
    }
    const literal = /^(-?\d+(\.\d+)?([eE][+-]?\d+)?|true|false|null)/.exec(text.slice(at))[0];
    at += literal.length;
    if (/^-?\d+$/.test(literal)) {
      const big = BigInt(literal);
      return big >= BigInt(Number.MIN_SAFE_INTEGER) && big <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(big) : big;
    }
    return JSON.parse(literal);
  };
  return value();
}

// ---- postcard, as the server reads the handshake ----

class Writer {
  constructor() { this.bytes = []; }
  byte(b) { this.bytes.push(b & 0xff); }
  raw(bytes) { for (const b of bytes) this.bytes.push(b); }
  varint(n) {
    let value = BigInt(n);
    do {
      let b = Number(value & 0x7fn);
      value >>= 7n;
      if (value) b |= 0x80;
      this.byte(b);
    } while (value);
  }
  str(s) { const bytes = new TextEncoder().encode(s); this.varint(bytes.length); this.raw(bytes); }
  done() { return Uint8Array.from(this.bytes); }
}

const EXTENDED = {foreign: 1, agent: 4, stand_in: 8, issued: 16, server_key: 32, browser_pages: 64, workspace_service: 128};

function uuidBytes(text) {
  const hex = text.replaceAll('-', '');
  return Array.from({length: 16}, (_, i) => parseInt(hex.slice(i * 2, i * 2 + 2), 16));
}

/** `cert` (as the certificate's JSON reads, [`parseExact`]) in postcard. */
export function writeCert(w, cert) {
  w.raw(cert.user_public_key);
  w.raw(cert.device_public_key);
  w.str(cert.device_label);
  const caps = cert.capabilities;
  const issued = Boolean(cert.issuer);
  let extended = 0;
  for (const [name, bit] of Object.entries(EXTENDED)) {
    if (name === 'issued' ? issued : name === 'agent' ? caps.agent != null : caps[name]) extended |= bit;
  }
  const flags = [caps.can_own_workspaces ? 1 : 0, caps.can_admit_devices ? 1 : 0, 0, 0];
  if (extended & ~EXTENDED.foreign) {
    w.byte(3); w.byte(extended);
    if (caps.agent != null) { if (caps.agent === 'all') w.byte(0); else { w.byte(1); w.raw(uuidBytes(caps.agent)); } }
  } else if (caps.foreign) {
    w.byte(2);
  }
  w.raw(flags);
  w.varint(cert.issued_at_ns);
  if (cert.expires_at_ns == null) w.byte(0); else { w.byte(1); w.varint(cert.expires_at_ns); }
  w.varint(cert.signature.length); w.raw(cert.signature);
  if (issued) writeCert(w, cert.issuer);
}

/** The hello a device opens its session with: its protocol version, its
 * certificate, its challenge and no entry. */
export function hello(protocol, cert, nonce) {
  const w = new Writer();
  w.varint(protocol);
  writeCert(w, cert);
  w.raw(nonce);
  w.byte(0);
  return w.done();
}

class Reader {
  constructor(bytes) { this.bytes = bytes; this.at = 0; }
  byte() { if (this.at >= this.bytes.length) throw new RangeError('short'); return this.bytes[this.at++]; }
  take(n) { if (this.at + n > this.bytes.length) throw new RangeError('short'); const out = this.bytes.slice(this.at, this.at + n); this.at += n; return out; }
  varint() { let shift = 0n, value = 0n, b; do { b = this.byte(); value |= BigInt(b & 0x7f) << shift; shift += 7n; } while (b & 0x80); return value; }
}

function skipCert(r) {
  r.take(64);
  r.take(Number(r.varint()));
  const first = r.byte();
  let issued = false;
  if (first === 3) {
    const extended = r.byte();
    issued = Boolean(extended & EXTENDED.issued);
    if (extended & EXTENDED.agent && r.byte() === 1) r.take(16);
    r.take(4);
  } else if (first === 2) {
    r.take(4);
  } else {
    r.take(3);
  }
  r.varint();
  if (r.byte()) r.varint();
  r.take(Number(r.varint()));
  if (issued) skipCert(r);
}

/** The server's challenge from its ack: what this device signs back. */
export function serverNonce(ack) {
  const r = new Reader(ack);
  r.varint();
  skipCert(r);
  return r.take(32);
}

/** The third message: this device's signature over the server's challenge. */
export async function ack2(sign, nonce) {
  const digest = blake3(Uint8Array.from([...CLIENT_TAG, ...nonce]));
  const signature = await sign(digest);
  const w = new Writer();
  w.varint(signature.length); w.raw(signature);
  return w.done();
}

// ---- An act, signed by the person's device ----
// What the device serving the page checks before the act runs, and every holder of the change it
// makes checks again (`signed_invocation.rs`): the statement's exact bytes, its BLAKE3, signed
// under the invocation domain.

const INVOCATION_DOMAIN = new TextEncoder().encode('tilo-invocation-v1\0');
const ARGUMENTS_DOMAIN = new TextEncoder().encode('tilo-invocation-arguments-v1\0');

/** `value` as JSON with every object's keys sorted: the form the arguments are hashed in. */
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

/** `data`'s bytes as lowercase hex, two digits each: how the wire spells a key, a hash or a signature. */
const hex = data => Array.from(data, b => b.toString(16).padStart(2, '0')).join('');

function u64(n) {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(n), true);
  return out;
}

/** The call `device` signs for `act` (`{id, target, action}`) on the page of `app` in
 * `workspace`: the signed statement, as the wire carries it, with the act as its argument.
 * Its issue time is a whole second, which a number holds exactly. */
export async function signedAct(device, workspace, app, act,
  {invocation = crypto.randomUUID(), issuedAtNs = Math.floor(Date.now() / 1000) * 1e9} = {}) {
  const encoder = new TextEncoder();
  const bytes = [];
  const text = value => { const encoded = encoder.encode(value); bytes.push(...u64(encoded.length), ...encoded); };
  const arguments_ = blake3(Uint8Array.from([...ARGUMENTS_DOMAIN, 2, ...encoder.encode(canonical(act))]));
  const signer = Uint8Array.from(device.cert.device_public_key);
  text(workspace);
  bytes.push(...uuidBytes(invocation));
  text(app); text(app); text('act');
  bytes.push(...arguments_, 0, ...u64(issuedAtNs), ...signer);
  const signature = await device.sign(Uint8Array.from([...INVOCATION_DOMAIN, ...blake3(Uint8Array.from(bytes))]));
  const cert = new Writer();
  writeCert(cert, device.cert);
  return {
    signed: {
      workspace, invocation, target: {provider: app, address: {resource: app, action: 'act'}},
      arguments: hex(arguments_), on_behalf_of: null, issued_at_ns: issuedAtNs,
      signer: hex(signer), signature: hex(signature), signer_cert: hex(cert.done()),
    },
    argument: {Json: act},
  };
}

// ---- Frames: a little-endian u32 length, then the bytes ----

function framed(bytes) {
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length, true);
  out.set(bytes, 4);
  return out;
}

function frames(readable) {
  const reader = readable.getReader();
  let buffered = new Uint8Array(0);
  return async function next() {
    for (;;) {
      if (buffered.length >= 4) {
        const length = new DataView(buffered.buffer, buffered.byteOffset).getUint32(0, true);
        if (length > MAX_FRAME) throw new Error('frame too large');
        if (buffered.length >= 4 + length) {
          const frame = buffered.slice(4, 4 + length);
          buffered = buffered.slice(4 + length);
          return frame;
        }
      }
      const {value, done} = await reader.read();
      if (done) return null;
      const joined = new Uint8Array(buffered.length + value.length);
      joined.set(buffered); joined.set(value, buffered.length);
      buffered = joined;
    }
  };
}

// ---- This browser's devices ----

/** The device keys this browser holds that it kept a certificate of: each
 * `{cert, sign}`, newest certificate first. */
async function devices() {
  const {certifiedDeviceKeys} = await siteModule('device-key.mjs');
  const found = (await certifiedDeviceKeys()).map(({certificate, privateKey}) => ({
    cert: parseExact(certificate).cert,
    sign: async digest => new Uint8Array(await crypto.subtle.sign({name: 'Ed25519'}, privateKey, digest)),
  }));
  return found.sort((a, b) => (BigInt(b.cert.issued_at_ns) > BigInt(a.cert.issued_at_ns) ? 1 : -1));
}

// ---- The session ----

/** Open a session at the server and prove `device` on it: `{send, next, close}`,
 * or `null` when the server refused the device. */
async function session(config, device) {
  const {origin, path, certificate_sha256: pinned} = config.link;
  const options = pinned ? {serverCertificateHashes: [{algorithm: 'sha-256', value: Uint8Array.from(pinned)}]} : {};
  const transport = new WebTransport(origin + path, options);
  await transport.ready;
  const stream = await transport.createBidirectionalStream();
  const writer = stream.writable.getWriter();
  const next = frames(stream.readable);
  const nonce = crypto.getRandomValues(new Uint8Array(32));
  await writer.write(framed(hello(config.protocol, device.cert, nonce)));
  const ack = await next();
  // An empty frame stands before a refusal; a short one is the server's
  // version alone. Either way this device is not let in.
  if (!ack || ack.length < 64) { transport.close(); return null; }
  await writer.write(framed(await ack2(device.sign, serverNonce(ack))));
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  return {
    send: ask => writer.write(framed(encoder.encode(JSON.stringify(ask)))),
    next: async () => { const frame = await next(); return frame && JSON.parse(decoder.decode(frame)); },
    close: () => transport.close(),
    closed: transport.closed,
  };
}

// ---- This browser's person ----
// The site's own modules are served above the page's address (`<entry>w/<workspace>`), under the
// version directory `version.json` names: the person's root lives there (`identity.mjs`), which
// is what approves a browser waiting to be signed in, and the browser's device keys do too
// (`device-key.mjs`). A page pressed before it is in waits on it.
const modules = new Map();
function siteModule(name) {
  if (!modules.has(name)) {
    modules.set(name, (async () => {
      const at = location.pathname.indexOf('/w/');
      const base = new URL(at < 0 ? '/' : location.pathname.slice(0, at + 1), location.href);
      const response = await fetch(new URL('version.json', base), {cache: 'no-store'});
      if (!response.ok) throw new Error(`the site answered ${response.status}`);
      const {version} = await response.json();
      return import(new URL(`v/${version}/${name}`, base).href);
    })().catch(error => {
      modules.delete(name);
      throw error;
    }));
  }
  return modules.get(name);
}

// ---- The person's notifications, while the page is not in front ----
// A notice an app sends on one of the person's devices is kept in the record of their devices while
// it is shown, and every device of theirs shows it from there (the server sends them over this
// page's stream). A page nobody looks at shows nothing of its view, so it raises them as browser
// notifications instead; one in front has the page itself. What the person does with one here —
// clicking it, or dismissing it — is an answer in the record, so it leaves every device of theirs.

/** The browser notifications a page raises for the notices the person's devices show, while it is
 * not in front. `follow(list)` is what the record holds now: a notice in it is raised, one that
 * left it is taken away, and while the page is in front none is shown at all.
 * `visibilityChanged()` is the page coming in front or going away. `Notification` is the browser's
 * own, `hidden()` whether the page is not in front, `answer(id)` says the person is done with a
 * notice in the record, and `focus()` brings the page in front. */
export function browserNotices({hidden, Notification: Kind, answer, focus}) {
  // The notification each notice is raised as, by the record's id.
  const raised = new Map();
  // What the person here is done with: the record may show it for another moment, and a notice
  // they just dismissed is not raised again.
  const answered = new Set();
  let notices = [];
  // Take one away here without answering it: its person did not touch it.
  const take = id => {
    const notification = raised.get(id);
    if (!notification) return;
    raised.delete(id);
    // Ours, so the browser's closing of it is not the person's.
    notification.onclose = null;
    notification.close?.();
  };
  const raise = notice => {
    const notification = new Kind(notice.title || notice.app_name, {
      body: notice.body,
      tag: notice.id,
      requireInteraction: notice.urgency >= 2,
    });
    raised.set(notice.id, notification);
    const personDone = () => {
      answered.add(notice.id);
      answer(notice.id);
    };
    notification.onclick = () => {
      focus();
      take(notice.id);
      personDone();
    };
    // The person dismissed it from the browser's own list: an answer like any other.
    notification.onclose = () => {
      raised.delete(notice.id);
      personDone();
    };
  };
  const follow = list => {
    notices = list;
    if (!hidden()) {
      for (const id of [...raised.keys()]) take(id);
      return;
    }
    const there = new Set(list.map(notice => notice.id));
    for (const id of [...answered]) if (!there.has(id)) answered.delete(id);
    for (const id of [...raised.keys()]) if (!there.has(id)) take(id);
    for (const notice of list) {
      if (raised.has(notice.id) || answered.has(notice.id) || Kind.permission !== 'granted') continue;
      // A browser that refuses them anyway says nothing useful to the person here, and the page
      // goes on without: it is not worth the page.
      try {
        raise(notice);
      } catch (error) {
        console.warn('notices: the browser showed none', error);
      }
    }
  };
  const visibilityChanged = () => {
    if (!hidden()) {
      for (const id of [...raised.keys()]) take(id);
      return;
    }
    follow(notices);
  };
  return {follow, visibilityChanged};
}

// ---- The page ----

function start() {
  const config = JSON.parse(document.getElementById('tilo-page-config').textContent);
  const page = document.getElementById('tilo-page');
  const style = document.getElementById('tilo-page-css');
  const base = `/w/${config.address.workspace}` + (config.address.app ? `/${config.address.app}` : '');
  // Where this host's own routes are reached over HTTP: its public origin, which the page asks
  // who its person is (the site's modules) and what waits for them to approve.
  const peer = typeof config.peer === 'string' && config.peer ? config.peer : null;
  // The workspace this page is, as the entry that handed the browser over
  // named it: an address carries no name.
  let workspaceName = null;
  try {
    const kept = JSON.parse(sessionStorage.getItem('tilo-page-workspace') ?? 'null');
    if (kept && kept.workspace === config.address.workspace && kept.name) {
      workspaceName = kept.name;
      document.documentElement.dataset.workspace = workspaceName;
    }
  } catch { /* none kept */ }
  let link = null;
  // The device the page's session is in as, which signs its acts.
  let signer = null;
  let composing = false;
  let unanswered = 0;
  let lastSelection = '';
  // Whether the page answers a Back or Forward of the browser's rather than
  // the person.
  let restoring = true;
  // The app's name, which a page with something open title-cases with what is open.
  let appTitle = config.address.app;
  const settled = new Map();
  // What was asked and not answered yet, in the order it was sent: an ask the
  // server answered by opening the document again is sent again.
  const sent = [];

  // ---- The page's call ----
  // A view that shows a call asks the browser to carry it: its microphone, camera, screen and
  // the workspace's live session are here, not where the view is laid out. The peer is opened
  // the first time a view asks, as this browser's certified device, and its voice is the
  // browser's own call (web/voice-peer.mjs, web/voice.mjs). A step this browser cannot carry is
  // refused in its own words, never left unanswered.
  let callPeer = null;
  let callDeviceKey = null;
  let callDriver = null;
  // One bootstrap at a time, and the steps in the order the view asked for them: a join before
  // the watch that reads it.
  let callsStarted = null;
  let callQueue = Promise.resolve();
  const answerCall = answer => {
    if (!link) return;
    link.send({call_result: answer}).catch(() => {
      page.dataset.callsLost = String(Number(page.dataset.callsLost ?? 0) + 1);
    });
  };
  // What this page's call counted, as the browser's own call keeps it (`web/voice.mjs`'s
  // `stats()`): the frames it heard and sent and the pictures it received. A walk that has to
  // know a call was carried reads these rather than what the call drew. Null until a view asks
  // the page for its call, and after the page is left (pagehide closes it).
  globalThis.tiloCallStats = () => {
    try { return callPeer?.voice?.call?.stats?.() ?? null; } catch { return null; }
  };
  const pageCalls = () => {
    callsStarted ??= (async () => {
      const {origin} = config.link;
      const {chosenDisplayName, keptMembership} = await siteModule('workspaces.mjs');
      const membership = keptMembership({host: {origin}});
      if (!membership?.userPublicKey) throw new Error('this browser kept no membership here');
      const {connectPeer, identityProfile} = await siteModule('peer.mjs');
      const {idbStorage, openDeviceKey} = await siteModule('device-key.mjs');
      const profile = identityProfile({userPublicKey: membership.userPublicKey}, origin);
      callDeviceKey = await openDeviceKey({profile, storage: idbStorage()});
      callPeer = await connectPeer({
        origin,
        // The page's own workspace: a browser kept in several of the host's workspaces carries
        // the call of the one this page shows.
        workspace: config.address.workspace,
        userPublicKey: membership.userPublicKey,
        profile,
        deviceKey: callDeviceKey,
        displayName: chosenDisplayName(membership.userPublicKey) ?? '',
        live: true,
        // The call goes on for a device this browser already certified: a page never pairs one.
        requireExistingCertificate: true,
      });
      const {createPageCalls} = await siteModule('page-calls.mjs');
      callDriver = createPageCalls({peerVoice: callPeer.voice, send: answerCall});
      return callDriver;
    })().catch(error => {
      // Tried once: a page that cannot carry a call says so to each step, rather than opening a
      // peer again on every one.
      console.warn('calls: the page could not open its call', error);
      return null;
    });
    return callsStarted;
  };
  const carryCall = step => {
    callQueue = callQueue
      .then(() => pageCalls())
      .then(driver => {
        if (driver) {
          driver.apply(step);
        } else {
          answerCall({
            request_id: step.request_id,
            answer: {refused: 'This browser cannot carry the call here.'},
          });
        }
      })
      .catch(error => console.warn('calls: the page could not carry a step', error));
  };
  const closeCalls = () => {
    callDriver?.dispose();
    callDriver = null;
    callPeer?.dispose?.();
    callPeer = null;
    callDeviceKey?.close?.();
    callDeviceKey = null;
    callsStarted = null;
  };
  addEventListener('pagehide', closeCalls);

  const say = (state, words) => {
    page.dataset.state = state;
    page.textContent = words ?? '';
  };

  const addressOf = () => {
    const rest = location.pathname.slice(base.length).replace(/^\//, '');
    return {...config.address, show: rest ? decodeURIComponent(rest) : null};
  };

  // An event the page has heard nothing back for shows the page at work,
  // quietly: the person keeps typing and clicking meanwhile. What it asked
  // and what it heard back are counted on the page, for whoever looks into a
  // page that shows a person's typing and keeps none of it.
  let working = null;
  let asked = 0;
  let heard = 0;
  const ask = (value, tries = 0) => {
    if (!link) return;
    unanswered++;
    page.dataset.asks = String(++asked);
    working ??= setTimeout(() => { page.dataset.working = ''; }, 300);
    sent.push({value, tries});
    link.send(value).catch(() => {
      page.dataset.asksLost = String(Number(page.dataset.asksLost ?? 0) + 1);
    });
  };
  // The app was still working on what was asked past the time the server
  // waits: it opened the document again from what the workspace holds, and
  // nothing asked of it since that request was applied. An ask cannot land
  // on a document no worker has rendered, so the page is shown again where
  // it is, quietly — the field the person is typing in keeps its text, its
  // caret and its focus — and what they asked for is asked again from there.
  // A click that opened nothing would otherwise be lost: the page shows
  // what the workspace holds, not what the person pressed.
  let redrawing = false;
  // What a reopen lost, asked again once the page is back: a click or a
  // choice in the order it was made, and for a field, only what the field
  // holds last — its earlier values are text it has since grown past, and
  // asking them again would put a shorter one over the longer.
  let redone = [];
  const lost = act => {
    if (act && (act.action?.value !== undefined || act.action?.edit !== undefined)) {
      redone = redone.filter((kept) => kept.id !== act.id);
    }
    redone.push(act);
  };
  const reopened = asked => {
    if (!link) return;
    // Why the page is shown again, for whoever looks into it.
    if (asked) page.dataset.reopened = 'asked again';
    // A page asked for again is asked for at most twice: one that keeps
    // reopening is not shown here.
    if (asked?.value?.open) {
      if (asked.tries >= 2) { say('refused', 'This page is not available here.'); return; }
      ask(asked.value, asked.tries + 1);
      return;
    }
    if (asked?.value?.act) lost(asked.value.act);
    if (redrawing) return;
    redrawing = true;
    ask({open: addressOf()});
  };
  const caughtUp = () => {
    if (unanswered > 0) return;
    clearTimeout(working);
    working = null;
    delete page.dataset.working;
  };

  const numberOf = element => Number(element.closest('[data-p]')?.dataset.p);

  // An act on an app's page goes signed by the device this page is in as: whose the change it
  // makes is. Acts are signed one after another, so they are asked in the order they were made.
  let signing = Promise.resolve();
  const act = (element, action) => {
    const id = numberOf(element);
    if (!Number.isFinite(id)) return;
    const asked = {id, target: element.dataset.t ?? null, action};
    const by = signer;
    if (!config.address.app || !by) { ask({act: asked}); return; }
    signing = signing.then(async () => {
      const signed = await signedAct(by, config.address.workspace, config.address.app, asked).catch(error => {
        console.warn('page: signing an act', error);
        return null;
      });
      ask({act: signed ? {...asked, signed} : asked});
    });
  };

  const editing = element => element?.matches?.('input,textarea,[contenteditable]') ?? false;
  const valueOf = element => (element.matches('input,textarea') ? element.value : element.innerText.replace(/\n$/, ''));
  const keyOf = element => element.dataset.p ? `[data-p="${element.dataset.p}"]`
    : element.dataset.block ? `[data-block="${CSS.escape(element.dataset.block)}"]` : null;

  // A document's editable blocks are edited in place, as the person's text.
  const editable = root => {
    for (const block of root.querySelectorAll('[data-a=edit]')) block.contentEditable = 'plaintext-only';
  };

  const sameAttributes = (to, from) => {
    for (const {name} of [...to.attributes]) if (!from.hasAttribute(name) && name !== 'contenteditable') to.removeAttribute(name);
    for (const {name, value} of from.attributes) if (to.getAttribute(name) !== value) to.setAttribute(name, value);
  };

  // Replace `old` with `fresh`, keeping the field being typed in where it is:
  // the same element, so its caret, its composition and its undo stay.
  const morph = (old, fresh, active) => {
    if (old === active) {
      sameAttributes(old, fresh);
      settled.set(old, fresh.matches('input,textarea') ? fresh.value : fresh.textContent);
      return;
    }
    const keep = [...old.children].find(child => child.contains(active));
    const key = keep && keyOf(keep);
    const kept = key && [...fresh.children].find(child => child.matches(key));
    if (!kept) {
      old.replaceWith(fresh);
      return;
    }
    sameAttributes(old, fresh);
    for (const child of [...old.childNodes]) if (child !== keep) child.remove();
    const children = [...fresh.childNodes];
    const at = children.indexOf(kept);
    keep.before(...children.slice(0, at));
    keep.after(...children.slice(at + 1));
    morph(keep, kept, active);
  };

  const element = id => page.querySelector(`[data-p="${id}"]`);
  const fromHtml = html => {
    const template = document.createElement('template');
    template.innerHTML = html;
    editable(template.content);
    return template.content.firstElementChild;
  };

  // Each step in order: an element replaced, put in as a child at its place, or taken out. What
  // comes or goes is the one element, so everything else stays the element the browser holds,
  // with its scroll, its focus and the motion it is in.
  const patch = patches => {
    const active = document.activeElement;
    for (const step of patches) {
      if (step.replace) {
        const old = element(step.replace.id);
        if (!old) continue;
        const fresh = fromHtml(step.replace.html);
        if (editing(active) && old.contains(active)) morph(old, fresh, active);
        else old.replaceWith(fresh);
      } else if (step.insert) {
        const parent = element(step.insert.parent);
        if (!parent) continue;
        parent.insertBefore(fromHtml(step.insert.html), parent.children[step.insert.at] ?? null);
      } else if (step.remove) {
        element(step.remove.id)?.remove();
      }
    }
    reconcile();
    focusAsked();
    follow();
  };

  // Where the view puts the focus (a new note's title), the page does too.
  // The element's number is the same for the title of every note, so what
  // was asked for before says nothing about a note opened since: the view's
  // focus is taken whenever the person is not already in a field of their
  // own. A title nobody's caret reached takes no typing, and the keys a
  // person types land on the app's shortcuts instead.
  const focusAsked = () => {
    // A field a patch brought in to be typed in (the first of an action's
    // fields) goes before the view's own.
    const focus = page.querySelector('[data-focus]')?.dataset.focus ?? null;
    const focused = page.querySelector('[autofocus]') ?? (focus && page.querySelector(`[data-p="${focus}"]`));
    if (!focused || !editing(focused) || document.activeElement === focused) return;
    const active = document.activeElement;
    if (active && page.contains(active) && editing(active)) return;
    focused.focus({preventScroll: true});
  };

  // The thing the view has open (the selected note) is the page's address:
  // what the person opened is a step Back returns from; what Back, Forward or
  // the first load showed only names where the page now is.
  const follow = () => {
    const selected = page.querySelector('[data-a=activate][data-t][aria-selected=true]');
    const target = selected?.dataset.t;
    // What the person has open names the tab and the step history keeps, as a
    // window's title does: two notes are two tabs, and Back names each one.
    const name = selected && (selected.getAttribute('aria-label') || selected.innerText.split('\n')[0].trim());
    const title = name ? `${name} — ${appTitle}` : appTitle;
    if (title !== document.title) document.title = title;
    const url = target ? `${base}/${encodeURIComponent(target)}` : location.pathname;
    if (url !== location.pathname) {
      if (restoring) history.replaceState(history.state, '', url);
      else history.pushState({tilo: true}, '', url);
    }
    restoring = false;
  };

  // Once nothing typed waits for its answer, a field shows what the server
  // holds: another person's edit, or what the app made of the typing.
  const reconcile = () => {
    if (composing || unanswered > 0) return;
    for (const [field, value] of settled) {
      settled.delete(field);
      if (!field.isConnected || valueOf(field) === value) continue;
      if (field.matches('input,textarea')) field.value = value;
      else field.textContent = value;
    }
  };

  // The field the person is typing in, its text and the caret's place: a page
  // that replaces itself under them puts them back as they were, so no key
  // they pressed is lost.
  const typingHeld = () => {
    const active = document.activeElement;
    const key = active && page.contains(active) && editing(active) && keyOf(active);
    if (!key) return null;
    return {key, value: valueOf(active), start: active.selectionStart, end: active.selectionEnd};
  };
  const typingBack = held => {
    const field = held && page.querySelector(held.key);
    if (!field) return;
    if (field.matches('input,textarea')) {
      field.value = held.value;
      if (held.start != null) field.setSelectionRange(held.start, held.end);
    } else {
      field.textContent = held.value;
    }
    field.focus({preventScroll: true});
  };

  // The view lays itself out at the page's size, as a window does at its own, so where its
  // widgets stand is where the page shows them: told when the page is shown and when the
  // browser's window changes size.
  let sized = null;
  let sizing = null;
  const resized = () => {
    const width = Math.round(page.clientWidth || innerWidth);
    const height = Math.round(page.clientHeight || innerHeight);
    const said = `${width}x${height}`;
    if (said === sized || !width || !height) return;
    sized = said;
    ask({act: {id: 0, target: null, action: {resize: {width, height}}}});
  };
  addEventListener('resize', () => {
    clearTimeout(sizing);
    sizing = setTimeout(resized, 150);
  });

  const show = ({title, css, body}) => {
    const held = typingHeld();
    appTitle = title;
    // The workspace's own page is named by the workspace; an app's by the app,
    // and by what is open in it (`follow`).
    document.title = config.address.app === '' && workspaceName ? workspaceName : title;
    style.textContent = css;
    delete page.dataset.state;
    page.innerHTML = body;
    editable(page);
    focusAsked();
    typingBack(held);
    sized = null;
    resized();
    // Shown again under a field being typed in: its text is sent from here,
    // so what the person wrote lands on the document they are looking at.
    const kept = held && page.querySelector(held.key);
    if (kept && (kept.dataset.a === 'value' || kept.dataset.a === 'edit')) act(kept, {value: held.value});
    follow();
    // The page says it arrived, as the entry it came from did: what a tool
    // waits for is the same marker on either.
    document.documentElement.dataset.arrival = 'done';
    if (!performance.getEntriesByName('tilo-page-shown').length) performance.mark('tilo-page-shown');
  };

  const answered = answer => {
    if (answer === 'loading') { say('loading'); return; }
    // A call step is the server's own question, not the answer to one the page asked: it keeps
    // the queue of asks where it is.
    if (answer?.call) { void carryCall(answer.call); return; }
    // What the person's devices show now, as the record of them holds it: raised as browser
    // notifications while the page is not in front, and taken away as they leave it.
    if (answer?.notices) {
      page.dataset.notices = String(answer.notices.length);
      noticesShown?.follow(answer.notices);
      return;
    }
    page.dataset.answers = String(++heard);
    if (answer.changed) { restoring = true; patch(answer.changed); return; }
    unanswered = Math.max(0, unanswered - 1);
    const asked = sent.shift();
    caughtUp();
    // Its document was opened again past a slow request: nothing the ask did
    // was applied, so the page is shown again where it is and what the person
    // did is asked again from there. Anything else the page refused is the
    // page saying it cannot do this now, which the person is told rather than
    // left with typing that was never kept.
    if (answer.refused?.reopened) {
      reopened(asked);
      return;
    }
    if (answer.shown) {
      redrawing = false;
      show(answer.shown);
      // What a reopen lost is asked again, in the order it was asked, now
      // that the page is back where the person left it.
      const again = redone;
      redone = [];
      for (const act of again) ask({act});
    } else if (answer.patched) patch(answer.patched);
    else if (answer.refused) {
      say('refused', 'This page is not available here.');
      // Why, for whoever looks into it: the reason is the server's word.
      page.dataset.refused = JSON.stringify(answer.refused);
    }
  };

  // A row that hands a link over: the link it carries was made before the
  // press, because a browser takes a clipboard write only inside the press
  // itself, and the host's answer comes after it. A browser that refuses the
  // write shows the link beside the row, to select.
  const handOver = element => {
    const text = element.dataset.copy;
    if (!text) return;
    const fallback = element.parentElement?.querySelector('.tilo-search-handover');
    const show = () => {
      if (!fallback) return;
      fallback.hidden = false;
      const code = fallback.querySelector('code');
      if (!code) return;
      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    };
    const writing = navigator.clipboard?.writeText?.(text);
    if (writing) writing.then(() => { if (fallback) fallback.hidden = true; }, show);
    else show();
  };

  // What the person does.
  page.addEventListener('click', event => {
    // A press that sets another device up hands the tab to the site's own
    // setup screen: it needs the person's identity, which the site's page
    // holds, not this one. The screen is the entry's and shows when this
    // browser's setup asks for it (`first-screen.mjs`'s `SETUP`), so the
    // press says the person is starting it now, and goes where it is served.
    const setup = event.target.closest('[data-setup]');
    if (setup && page.contains(setup)) {
      event.preventDefault();
      try {
        localStorage.setItem('tilo-setup', JSON.stringify({step: 'start', at: Date.now()}));
      } catch { /* No storage: the site's screen starts from its own state. */ }
      location.assign(setup.dataset.setup);
      return;
    }
    const element = event.target.closest('[data-a]');
    if (!element || !page.contains(element)) return;
    const action = element.dataset.a;
    if (action === 'activate') {
      // A link that names an address opens it: the browser's own step, as a link on any
      // page. A link naming the view's target (`data-t`) is an ask, as a press on a row is.
      if (element.tagName === 'A' && element.getAttribute('href') && !element.dataset.t) return;
      event.preventDefault();
      // What is typed next is not for the field the person clicked away from.
      const active = document.activeElement;
      if (editing(active) && !element.contains(active)) active.blur();
      handOver(element);
      act(element, 'activate');
    } else if (action === 'toggle' && !element.matches('input')) {
      event.preventDefault();
      act(element, 'toggle');
    } else if (action === 'value' && element.dataset.v !== undefined) {
      act(element, {value: element.dataset.v});
    } else if (action === 'back') {
      event.preventDefault();
      // The column behind the one in front comes to the front, as a narrow
      // window shows it. A page with nothing behind it is a step of the
      // browser's to go back to, if this page put one there.
      const front = page.querySelector('[data-pane][data-front]');
      const behind = front && [...front.parentElement.querySelectorAll(':scope>[data-pane]')].filter(pane => pane !== front).pop();
      if (behind) { delete front.dataset.front; behind.dataset.front = ''; }
      else if (history.state?.tilo) history.back();
    }
  });
  // A form's Enter runs what its own button runs; the browser never sends it.
  // A slider steps with its arrow keys, and an element that acts without being a button
  // presses with Enter or Space, as a native control does.
  page.addEventListener('keydown', event => {
    const element = event.target;
    if (!element?.dataset || !page.contains(element) || editing(element)) return;
    const action = element.dataset.a;
    if (action === 'slider') {
      const step = {ArrowRight: 'increment', ArrowUp: 'increment', ArrowLeft: 'decrement', ArrowDown: 'decrement'}[event.key];
      if (!step) return;
      event.preventDefault();
      act(element, step);
    } else if ((action === 'activate' || action === 'toggle') && !element.matches('button,a')
      && (event.key === 'Enter' || event.key === ' ')) {
      event.preventDefault();
      act(element, action);
    }
  });
  page.addEventListener('submit', event => {
    event.preventDefault();
    const run = event.target.querySelector('button[type=submit][data-a=activate]');
    if (run) act(run, 'activate');
  });
  page.addEventListener('change', event => {
    const element = event.target;
    if (element.dataset?.a === 'toggle') act(element, 'toggle');
  });
  page.addEventListener('input', event => {
    const element = event.target;
    if (composing || event.isComposing) return;
    if (element.dataset?.a === 'value' || element.dataset?.a === 'edit') act(element, {value: valueOf(element)});
  });
  page.addEventListener('compositionstart', () => { composing = true; });
  page.addEventListener('compositionend', event => {
    composing = false;
    const element = event.target;
    if (element.dataset?.a === 'value' || element.dataset?.a === 'edit') act(element, {value: valueOf(element)});
  });
  page.addEventListener('focusin', event => {
    const element = event.target;
    if (element.dataset?.a === 'value' || element.dataset?.a === 'edit') act(element, 'focus');
  });
  // Where the caret is matters to the app (a new block goes after it): told
  // when the person moves it, not on every key they type.
  const graphemes = (text, offset) => [...new Intl.Segmenter().segment(text.slice(0, offset))].length;
  const selected = () => {
    const element = document.activeElement;
    if (composing || element?.dataset?.a !== 'value' || !element.matches('input,textarea')) return;
    const text = element.value;
    const anchor = graphemes(text, element.selectionStart ?? 0);
    const focus = graphemes(text, element.selectionEnd ?? 0);
    const said = `${numberOf(element)}:${anchor}:${focus}`;
    if (said === lastSelection) return;
    lastSelection = said;
    act(element, {select: {anchor, focus}});
  };
  page.addEventListener('mouseup', () => setTimeout(selected));
  page.addEventListener('keyup', event => {
    if (/^(Arrow|Home|End|Page)/.test(event.key)) selected();
  });

  addEventListener('popstate', () => {
    restoring = true;
    ask({open: addressOf()});
  });

  // ---- The person's notifications ----
  // A page that is not in front shows the notices the person's devices hold as browser
  // notifications, and answers the ones its person dismisses there, so they leave every device of
  // theirs. The browser's permission is asked from a press of theirs, never on the page's own.
  const noticesShown = typeof globalThis.Notification === 'function' ? browserNotices({
    hidden: () => document.hidden,
    Notification: globalThis.Notification,
    answer: id => {
      if (link) link.send({answer_notice: {id}}).catch(() => {});
    },
    focus: () => globalThis.focus?.(),
  }) : null;
  const askToNotify = () => {
    if (!noticesShown || globalThis.Notification.permission !== 'default') return;
    globalThis.Notification.requestPermission()
      .then(() => noticesShown.visibilityChanged())
      .catch(() => {});
  };
  addEventListener('pointerdown', askToNotify, {once: true});
  addEventListener('keydown', askToNotify, {once: true});
  addEventListener('visibilitychange', () => noticesShown?.visibilityChanged());

  // ---- A browser of the person's waiting to be signed in ----
  // A tab that holds no root shows a ticket, and the person runs `tilo device add` with it on a
  // device of theirs. That device may hold no root either: it hands the signing on, and this
  // page, which holds the person's root, is where it happens. The page lists what waits for its
  // person (`identity.mjs`'s `signInsWaiting`, the host's `devices#pending` list) and asks about
  // one: the device's name and what certifying it makes it. The root signs, the host answers the
  // tab's ticket, and the tab reads its certificate when it follows it.
  let signInAsked = null;
  let personHeld = null;
  let personAbsent = false;
  const signInsAnswered = new Set();
  const signInBar = () => {
    let bar = document.getElementById('tilo-sign-in');
    if (bar) return bar;
    bar = document.createElement('div');
    bar.id = 'tilo-sign-in';
    bar.hidden = true;
    bar.setAttribute('role', 'alertdialog');
    bar.innerHTML = '<p></p><button type="button" id="tilo-sign-in-allow" data-sign-in-answer="allow">Sign in</button>'
      + '<button type="button" id="tilo-sign-in-deny" data-sign-in-answer="deny">Not now</button>';
    bar.addEventListener('click', event => {
      const answer = event.target.closest('[data-sign-in-answer]');
      if (!answer) return;
      event.preventDefault();
      answerSignInShown(answer.dataset.signInAnswer === 'allow');
    });
    document.body.append(bar);
    return bar;
  };
  const showSignIn = request => {
    signInAsked = request;
    const bar = signInBar();
    bar.dataset.signIn = request.label;
    bar.querySelector('p').textContent =
      `Sign “${request.label}” in? It becomes one of your devices: ${request.capability}.`;
    for (const button of bar.querySelectorAll('button')) button.hidden = false;
    bar.hidden = false;
  };
  const hideSignIn = () => {
    signInAsked = null;
    const bar = document.getElementById('tilo-sign-in');
    if (!bar) return;
    bar.hidden = true;
    delete bar.dataset.signIn;
  };
  const answerSignInShown = async approved => {
    const request = signInAsked;
    if (!request) return;
    signInsAnswered.add(request.digest);
    const bar = document.getElementById('tilo-sign-in');
    // The bar says how it ended in place of the answer, then goes.
    for (const button of bar?.querySelectorAll('button') ?? []) button.hidden = true;
    try {
      const id = await siteModule('identity.mjs');
      const host = await id.approvalHost(peer);
      await id.answerSignIn(host, personHeld, request, approved);
      signInAsked = null;
      if (bar) {
        bar.querySelector('p').textContent = approved
          ? `“${request.label}” is signed in.`
          : `“${request.label}” was not signed in.`;
        setTimeout(hideSignIn, 4000);
      }
    } catch (error) {
      // Answered nowhere: the next look asks again.
      signInsAnswered.delete(request.digest);
      if (bar) {
        for (const button of bar.querySelectorAll('button')) button.hidden = false;
        bar.querySelector('p').textContent = 'That did not work. Try again.';
      }
      console.warn('sign-in: answering the waiting browser', error);
    }
  };
  const lookForSignIns = async () => {
    if (!peer || signInAsked || personAbsent) return;
    try {
      const id = await siteModule('identity.mjs');
      if (!personHeld) {
        // Opening the person reads the runtime; a browser that keeps nobody stops here, cheaply.
        if (!(await id.keptIdentity().catch(() => null))?.rootPublicKey) { personAbsent = true; return; }
        personHeld = await id.browserIdentity({create: false}).catch(() => null);
        if (!personHeld) return;
      }
      const host = await id.approvalHost(peer);
      const waiting = await id.signInsWaiting(host, personHeld).catch(() => []);
      const next = waiting.find(request => !signInsAnswered.has(request.digest));
      if (next) showSignIn(next);
    } catch (error) {
      // The host not reached, its modules not in: the next look tries again.
      console.warn('sign-in: the browsers waiting', error);
    }
  };
  lookForSignIns();
  setInterval(lookForSignIns, 2000);

  const connect = async () => {
    if (!config.link || !globalThis.WebTransport) { say('refused', 'This browser cannot open this page.'); return; }
    const found = await devices().catch(error => { console.warn("the site's modules", error); return []; });
    if (!found.length) {
      say('refused', 'Sign in to Tilo in this browser to open this page.');
      return;
    }
    for (const device of found) {
      const opened = await session(config, device).catch(() => null);
      if (!opened) continue;
      link = opened;
      signer = device;
      ask({open: addressOf()});
      let answer;
      while ((answer = await link.next().catch(() => null))) {
        if (answer.refused === 'not_member' && !page.querySelector('[data-p]')) break;
        answered(answer);
      }
      link.close();
      link = null;
      signer = null;
      unanswered = 0;
      sent.length = 0;
      redone.length = 0;
      // The record no longer reaches this page: nothing of it is shown here.
      noticesShown?.follow([]);
      caughtUp();
      if (page.querySelector('[data-p]')) {
        // The session ended under an open page: open it again.
        setTimeout(connect, 1000);
        return;
      }
    }
    if (!page.querySelector('[data-p]')) say('refused', 'This page is not available to you.');
  };
  say('opening');
  connect();
}

if (globalThis.document?.getElementById('tilo-page-config')) start();
