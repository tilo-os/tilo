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
const DEVICES = 'tilo-browser-devices';
// The name the store of device keys had before (`device-key.mjs`).
const STORE_BEFORE = 'custody';
const MAX_FRAME = 8 * 1024 * 1024;
// The element number the question sheet a host asks this person stands under:
// an app numbers its own elements from one, and the sheet is no app's
// (`tilo_server_protocol::page::QUESTION_PATCH_ID`).
const QUESTION_PATCH = '4294967295';

// ---- BLAKE3 of one block (the handshake's digests are 59 bytes) ----

const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const PERMUTE = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/** BLAKE3 of `input`, at most one 64-byte block: the digest a handshake signs. */
export function blake3Block(input) {
  if (input.length > 64) throw new RangeError('one block at most');
  const block = new Uint8Array(64);
  block.set(input);
  let m = Array.from({length: 16}, (_, i) => new DataView(block.buffer).getUint32(i * 4, true));
  // CHUNK_START | CHUNK_END | ROOT
  const v = [...IV, IV[0], IV[1], IV[2], IV[3], 0, 0, input.length, 1 | 2 | 8];
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
  const out = new Uint8Array(32);
  const view = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) view.setUint32(i * 4, (v[i] ^ v[i + 8]) >>> 0, true);
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

const EXTENDED = {foreign: 1, can_administer_devices: 2, agent: 4, stand_in: 8, issued: 16, server_key: 32, browser_pages: 64, workspace_service: 128};

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
  const digest = blake3Block(Uint8Array.from([...CLIENT_TAG, ...nonce]));
  const signature = await sign(digest);
  const w = new Writer();
  w.varint(signature.length); w.raw(signature);
  return w.done();
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

const hex = bytes => Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');

/** The device keys this browser holds that it kept a certificate of: each
 * `{cert, sign}`, newest certificate first. */
async function devices() {
  let exact = {};
  try { exact = JSON.parse(localStorage.getItem(DEVICES) ?? 'null')?.exact ?? {}; } catch { /* none kept */ }
  if (!Object.keys(exact).length) return [];
  const keys = await new Promise(resolve => {
    const request = indexedDB.open('tilo-device');
    // A browser that never had a device gets no database from this page.
    request.onupgradeneeded = () => request.transaction.abort();
    request.onerror = () => resolve([]);
    request.onsuccess = () => {
      const db = request.result;
      const store = ['device-key', STORE_BEFORE].find(name => db.objectStoreNames.contains(name));
      if (!store) { resolve([]); return; }
      const all = db.transaction(store).objectStore(store).getAll();
      all.onsuccess = () => resolve(all.result ?? []);
      all.onerror = () => resolve([]);
    };
  });
  const found = [];
  for (const key of keys) {
    if (key?.kind !== 'webcrypto-ed25519') continue;
    const text = exact[hex(key.publicKey)];
    if (!text) continue;
    const cert = parseExact(text).cert;
    found.push({cert, sign: async digest => new Uint8Array(await crypto.subtle.sign({name: 'Ed25519'}, key.privateKey, digest))});
  }
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
// is what approves a browser waiting to be signed in. A page pressed before it is in waits on it.
function siteBase() {
  const at = location.pathname.indexOf('/w/');
  return new URL(at < 0 ? '/' : location.pathname.slice(0, at + 1), location.href);
}
let identityLoading = null;
function identityModule() {
  identityLoading ??= (async () => {
    const base = siteBase();
    const response = await fetch(new URL('version.json', base), {cache: 'no-store'});
    if (!response.ok) throw new Error(`the site answered ${response.status}`);
    const {version} = await response.json();
    return import(new URL(`v/${version}/identity.mjs`, base).href);
  })().catch(error => {
    identityLoading = null;
    throw error;
  });
  return identityLoading;
}

// ---- The page ----

/** The workspace address of another app or one of its resources. */
export function workspaceItemPath(workspace, appId, item) {
  const app = appId.split('.').at(-1);
  if (!/^[a-zA-Z0-9-]+$/.test(app) || item === '.' || item === '..') return null;
  const path = `/w/${encodeURIComponent(workspace)}/${app}`;
  return item ? `${path}/${encodeURIComponent(item)}` : path;
}

/** Whether a replacement input still represents the password field being edited. */
export function sameSecretEntry(old, fresh) {
  return old.type === 'password' && fresh?.type === 'password' && Boolean(old.dataset.t) &&
    old.dataset.t === fresh.dataset.t && old.getAttribute('aria-label') === fresh.getAttribute('aria-label');
}

/** The mounted controls belong to the signed connection that showed them. */
export function authenticatedPage(page) {
  let link = null;
  let shown = false;
  return {
    connect(next) {
      link = null;
      shown = false;
      page.replaceChildren();
      link = next;
    },
    show(body) {
      if (!link) return;
      page.innerHTML = body;
      shown = true;
    },
    get shown() { return shown; },
    owns(element) { return shown && page.contains(element); },
    send(ask, element) {
      if (!link || (!ask.open && (!shown || (element && !page.contains(element))))) return null;
      return link.send(ask);
    },
  };
}

/** The same person-device and issuer rules that the host applies to paired devices. */
export function pagePerson(cert) {
  const personal = caps => caps.agent == null && !caps.stand_in && !caps.workspace_service;
  const issuer = cert.issuer;
  return personal(cert.capabilities) && (!issuer || issuer.capabilities.stand_in ||
    (!issuer.issuer && personal(issuer.capabilities))) ? hex(cert.user_public_key) : null;
}

/** What this root would authorize, before it signs. User-provided names are shown as text. */
export function approvalPrompt(request, workspaceName) {
  if (request.kind !== 'agent') return `Sign “${request.label}” in? It becomes one of your devices: ${request.capability}.`;
  const workspace = workspaceName ? `${workspaceName} (${request.workspace})` : request.workspace;
  const apps = request.apps.length ? request.apps.join(', ') : 'all apps';
  return [`Allow “${request.label}” (${request.agent}) to act for you in ${workspace}?`,
    `Access to ${apps} until ${new Date(request.expiresAtMs).toLocaleString()}.`,
    `Compare code ${request.pairingCode} with the code on the device that requested access.`].join('\n');
}

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
  const view = authenticatedPage(page);
  let signedInPerson = null;
  let deviceSession = null;
  let composing = false;
  let unanswered = 0;
  let lastSelection = '';
  // Whether the page answers a Back or Forward of the browser's rather than
  // the person.
  let restoring = true;
  // A resource link can open a row with a different local target, including a
  // new conversation. Keep its reloadable address until the selection changes.
  let addressedSelection = null;
  // The app's name, which a page with something open title-cases with what is open.
  let appTitle = config.address.app;
  const settled = new Map();
  // What was asked and not answered yet, in the order it was sent: an ask the
  // server answered by opening the document again is sent again.
  const sent = [];

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
  const ask = (value, tries = 0, replay = true, element) => {
    const sending = view.send(value, element);
    if (!sending) return;
    unanswered++;
    page.dataset.asks = String(++asked);
    working ??= setTimeout(() => { page.dataset.working = ''; }, 300);
    sent.push({value: replay ? value : null, tries});
    sending.catch(() => {
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

  const act = (element, action) => {
    if (!view.owns(element)) return;
    const id = numberOf(element);
    if (!Number.isFinite(id)) return;
    // A replacement view may have a different prompt at the same element number.
    ask({act: {id, target: element.dataset.t ?? null, action}}, 0, !secretEntry(element), element);
  };

  const editing = element => element?.matches?.('input,textarea,[contenteditable]') ?? false;
  const secretEntry = element => element?.matches?.('input[type=password]') ?? false;
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
      // Only the live browser input holds its characters. A patch of the same
      // field keeps its value and caret; a different field starts empty.
      if (secretEntry(old)) {
        if (sameSecretEntry(old, fresh)) sameAttributes(old, fresh);
        else old.replaceWith(fresh);
        return;
      }
      sameAttributes(old, fresh);
      settled.set(old, fresh.matches('input,textarea') ? fresh.value : fresh.textContent);
      return;
    }
    const keep = [...old.children].find(child => child.contains(active));
    const key = keep && keyOf(keep);
    const kept = key && [...fresh.children].find(child => child.matches(key));
    if (!kept) {
      // A prompt's layout may change around its input while the person types.
      const field = secretEntry(active) && keyOf(active) && fresh.querySelector(keyOf(active));
      if (field && sameSecretEntry(active, field)) {
        sameAttributes(active, field);
        field.replaceWith(active);
        old.replaceWith(fresh);
        active.focus({preventScroll: true});
        return;
      }
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

  const patch = patches => {
    const active = document.activeElement;
    for (const {id, html} of patches) {
      const old = page.querySelector(`[data-p="${id}"]`);
      if (!old) continue;
      const template = document.createElement('template');
      template.innerHTML = html;
      const fresh = template.content.firstElementChild;
      editable(template.content);
      if (editing(active) && old.contains(active)) morph(old, fresh, active);
      else old.replaceWith(fresh);
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
  const follow = (opened = false) => {
    const selected = page.querySelector('[data-a=activate][data-t][aria-selected=true]');
    const target = selected?.dataset.t;
    // What the person has open names the tab and the step history keeps, as a
    // window's title does: two notes are two tabs, and Back names each one.
    const name = selected && (selected.getAttribute('aria-label') || selected.innerText.split('\n')[0].trim());
    const title = name ? `${name} — ${appTitle}` : appTitle;
    if (title !== document.title) document.title = title;
    if (opened) addressedSelection = addressOf().show ? target : null;
    if (target !== addressedSelection) addressedSelection = null;
    const url = target && !addressedSelection ? `${base}/${encodeURIComponent(target)}` : location.pathname;
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
    if (secretEntry(active)) return null;
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

  const show = ({title, css, body}) => {
    const held = typingHeld();
    appTitle = title;
    // The workspace's own page is named by the workspace; an app's by the app,
    // and by what is open in it (`follow`).
    document.title = config.address.app === '' && workspaceName ? workspaceName : title;
    style.textContent = css;
    delete page.dataset.state;
    view.show(body);
    // Where a question the host asks this person stands over the app: the
    // sheet is patched in here, and empty until one comes.
    if (!page.querySelector(`[data-p="${QUESTION_PATCH}"]`)) {
      const holder = document.createElement('div');
      holder.dataset.p = QUESTION_PATCH;
      holder.className = 'tilo-question-sheet';
      holder.hidden = true;
      page.append(holder);
    }
    editable(page);
    focusAsked();
    typingBack(held);
    // Shown again under a field being typed in: its text is sent from here,
    // so what the person wrote lands on the document they are looking at.
    const kept = held && page.querySelector(held.key);
    if (kept && (kept.dataset.a === 'value' || kept.dataset.a === 'edit')) act(kept, {value: held.value});
    follow(true);
    // The page says it arrived, as the entry it came from did: what a tool
    // waits for is the same marker on either.
    document.documentElement.dataset.arrival = 'done';
    if (!performance.getEntriesByName('tilo-page-shown').length) performance.mark('tilo-page-shown');
  };

  const answered = answer => {
    if (answer === 'loading') { say('loading'); return; }
    page.dataset.answers = String(++heard);
    if (answer.changed) {
      if (view.shown) { restoring = true; patch(answer.changed); }
      return;
    }
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
    } else if (answer.patched && view.shown) patch(answer.patched);
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
    if (!view.owns(event.target)) return;
    const pair = event.target.closest('[data-pair-device]');
    if (pair && page.contains(pair)) {
      event.preventDefault();
      if (!signedInPerson) return;
      page.querySelector('[data-pair-error]')?.remove();
      pair.disabled = true;
      const person = signedInPerson;
      const signal = deviceSession?.signal;
      void (async () => {
        try {
          const id = await identityModule();
          await id.pairWorkspaceDevice({origin: peer, workspace: config.address.workspace,
            person, signal, base: siteBase().href});
        } catch (error) {
          if (signal.aborted || !view.owns(pair)) return;
          let message = page.querySelector('[data-pair-error]');
          if (!message) {
            message = document.createElement('p');
            message.dataset.pairError = '';
            message.setAttribute('role', 'alert');
            pair.after(message);
          }
          message.textContent = error.message || 'Could not open device pairing. Try again.';
        } finally {
          pair.disabled = false;
        }
      })();
      return;
    }
    // Connecting an agent or computer starts setup on the site's entry.
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
    if (element.dataset.openApp && !element.disabled) {
      event.preventDefault();
      const path = workspaceItemPath(config.address.workspace, element.dataset.openApp, element.dataset.openItem);
      if (path) location.assign(path);
      return;
    }
    const action = element.dataset.a;
    if (action === 'activate') {
      event.preventDefault();
      // What is typed next is not for the field the person clicked away from.
      const active = document.activeElement;
      if (editing(active) && !element.contains(active)) active.blur();
      handOver(element);
      act(element, 'activate');
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

  // This page's root answers sign-in tickets and agent grants. Only its certified session
  // in the requested workspace can review a grant. Device pairing keeps its explicit invite.
  let shownRequest = null;
  let personHeld = null;
  let personAbsent = false;
  let looking = false;
  let answering = false;
  let resultUntil = 0;
  const approvalWorkspace = () => view.shown && signedInPerson && personHeld?.userPublicKey === signedInPerson &&
    !personHeld.locked && deviceSession && !deviceSession.signal.aborted ? config.address.workspace : null;
  const approvalBar = () => {
    let bar = document.getElementById('tilo-sign-in');
    if (bar) return bar;
    bar = document.createElement('div');
    bar.id = 'tilo-sign-in';
    bar.hidden = true;
    bar.setAttribute('role', 'alertdialog');
    bar.innerHTML = '<p></p><button type="button" data-approval-answer="allow"></button>'
      + '<button type="button" data-approval-answer="deny"></button>';
    bar.addEventListener('click', event => {
      const answer = event.target.closest('[data-approval-answer]');
      if (!answer) return;
      event.preventDefault();
      void answerShown(answer.dataset.approvalAnswer === 'allow');
    });
    document.body.append(bar);
    return bar;
  };
  const showApproval = request => {
    shownRequest = request;
    const bar = approvalBar();
    const agent = request.kind === 'agent';
    delete bar.dataset.agentGrant;
    delete bar.dataset.signIn;
    if (agent) bar.dataset.agentGrant = request.agent;
    else bar.dataset.signIn = request.label;
    bar.setAttribute('aria-label', agent ? 'Assistant access request' : 'Browser sign-in request');
    bar.querySelector('p').textContent = approvalPrompt(request, workspaceName);
    const allow = bar.querySelector('[data-approval-answer="allow"]');
    const deny = bar.querySelector('[data-approval-answer="deny"]');
    allow.id = agent ? 'tilo-agent-grant-allow' : 'tilo-sign-in-allow';
    deny.id = agent ? 'tilo-agent-grant-deny' : 'tilo-sign-in-deny';
    allow.textContent = agent ? 'Approve' : 'Sign in';
    deny.textContent = agent ? 'Decline' : 'Not now';
    for (const button of bar.querySelectorAll('button')) button.hidden = false;
    bar.hidden = false;
  };
  const hideApproval = () => {
    shownRequest = null;
    const bar = document.getElementById('tilo-sign-in');
    if (!bar) return;
    bar.hidden = true;
    delete bar.dataset.signIn;
    delete bar.dataset.agentGrant;
  };
  const answerShown = async approved => {
    const request = shownRequest;
    if (!request || answering) return;
    answering = true;
    const bar = document.getElementById('tilo-sign-in');
    // The bar says how it ended in place of the answer, then goes.
    for (const button of bar?.querySelectorAll('button') ?? []) button.hidden = true;
    try {
      const id = await identityModule();
      const host = await id.approvalHost(peer);
      const root = await id.browserIdentity({create: false});
      if (request !== shownRequest) return;
      if (!root?.sign || root.locked || root.userPublicKey !== personHeld?.userPublicKey) {
        personHeld = null;
        hideApproval();
        return;
      }
      if (id.answerWorkspaceApproval) {
        await id.answerWorkspaceApproval(host, root, approvalWorkspace(), request, approved);
      } else {
        await id.answerSignIn(host, root, request.digest, approved);
      }
      if (request !== shownRequest) return;
      const result = request.kind === 'agent' ? (approved ? 'approved' : 'declined') :
        (approved ? 'signed in' : 'not signed in');
      bar.querySelector('p').textContent = `“${request.label}” was ${result}.`;
      resultUntil = Date.now() + 4000;
      shownRequest = null;
    } catch (error) {
      if (request !== shownRequest) return;
      showApproval(request);
      bar.querySelector('p').textContent += '\nCould not complete this request. Try again.';
      console.warn('workspace approval: could not answer', error);
    } finally {
      answering = false;
    }
  };
  const lookForApprovals = async () => {
    if (!peer || looking || answering || personAbsent || Date.now() < resultUntil) return;
    looking = true;
    try {
      const id = await identityModule();
      if (!personHeld) {
        // Opening the person reads the runtime; a browser that keeps nobody stops here, cheaply.
        if (!(await id.keptIdentity().catch(() => null))?.rootPublicKey) { personAbsent = true; return; }
        personHeld = await id.browserIdentity({create: false}).catch(() => null);
        if (!personHeld) return;
      }
      const host = await id.approvalHost(peer);
      const session = deviceSession;
      const waiting = id.workspaceApprovals
        ? await id.workspaceApprovals(host, personHeld, approvalWorkspace())
        : await id.signInsWaiting(host, personHeld);
      if (answering || Date.now() < resultUntil || session !== deviceSession) return;
      const next = waiting.find(request => request.kind !== 'agent' || request.workspace === approvalWorkspace());
      if (!next) hideApproval();
      else if (next.digest !== shownRequest?.digest || next.token !== shownRequest?.token) showApproval(next);
    } catch (error) {
      console.warn('workspace approval: could not read requests', error);
    } finally {
      looking = false;
    }
  };
  void lookForApprovals();
  setInterval(lookForApprovals, 2000);

  const connect = async () => {
    if (!config.link || !globalThis.WebTransport) { say('refused', 'This browser cannot open this page.'); return; }
    const found = await devices();
    if (!found.length) {
      say('refused', 'Sign in to Tilo in this browser to open this page.');
      return;
    }
    for (const device of found) {
      const opened = await session(config, device).catch(() => null);
      if (!opened) continue;
      view.connect(opened);
      signedInPerson = pagePerson(device.cert);
      deviceSession = new AbortController();
      ask({open: addressOf()});
      let answer;
      while ((answer = await opened.next().catch(() => null))) {
        if (answer.refused === 'not_member' && !view.shown) break;
        answered(answer);
      }
      const reconnect = view.shown;
      view.connect(null);
      opened.close();
      deviceSession.abort();
      if (shownRequest?.kind === 'agent') hideApproval();
      deviceSession = null;
      signedInPerson = null;
      settled.clear();
      composing = false;
      lastSelection = '';
      restoring = true;
      addressedSelection = null;
      redrawing = false;
      document.title = workspaceName || config.address.app;
      style.textContent = '';
      delete document.documentElement.dataset.arrival;
      unanswered = 0;
      sent.length = 0;
      redone.length = 0;
      caughtUp();
      if (reconnect) {
        // The session ended under an open page: open it again.
        say('opening');
        setTimeout(connect, 1000);
        return;
      }
    }
    say('refused', 'This page is not available to you.');
  };
  say('opening');
  connect();
}

if (globalThis.document?.getElementById('tilo-page-config')) start();
