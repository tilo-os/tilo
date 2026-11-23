import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  allowFields, denyFields, MAX_GRANT_LIFETIME_MS, pageLocale, readSignInRequest, requestWords, serviceWords,
  signInOrigin, siteHome, startSignInPage,
} from './sign-in.mjs';
import {runtimeFake} from './runtime-surface.mjs';
import {personRecord, PERSON_KEY} from './device-key.mjs';
import {unhex} from './identity.mjs';

const NOW = 1_800_000_000_000;
const CHALLENGE = 'a'.repeat(64);
const KEY = 'b'.repeat(64);
const WORKSPACE = '3f2a1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d';
const ROOT = 'c'.repeat(64);
const DAY = 24 * 60 * 60 * 1000;

/** The link a service writes for the person's browser, as given. */
function link(over = {}) {
  const params = {
    audience: 'https://entry.example',
    challenge: CHALLENGE,
    return: 'https://entry.example/sign-in',
    pending: 'req-1',
    client: 'Claude Code',
    grant_key: KEY,
    grant_scope: 'all',
    grant_expires: String(NOW + 7 * DAY),
    ...over,
  };
  return `?${new URLSearchParams(Object.entries(params).filter(([, value]) => value !== null)).toString()}`;
}

const read = (over, options = {now: NOW}) => readSignInRequest(link(over), options);

test('a service’s link asks what the route answers', () => {
  assert.deepEqual(read(), {
    audience: 'https://entry.example',
    host: 'entry.example',
    challenge: CHALLENGE,
    returnTo: 'https://entry.example/sign-in',
    pending: 'req-1',
    client: 'Claude Code',
    grantKey: KEY,
    grantScope: 'all',
    grantExpiresMs: NOW + 7 * DAY,
    grantServerKey: false,
    grantPages: false,
  });
});

test('a key that may operate web pages in Browser says so, and only a grant carries it', () => {
  const operating = read({grant_pages: '1'});
  assert.equal(operating.grantPages, true);
  assert.match(requestWords(operating).pages, /without asking you each time/);
  assert.match(requestWords(operating).pages, /Submitting still asks you/);
  assert.match(requestWords(read()).pages, /asks you before every page action/);
  // Nothing that asks for no grant carries the flag.
  assert.equal(read({grant_key: null, grant_scope: null, grant_expires: null, grant_pages: '1'}).grantPages, false);
});

test('a server key reaches all workspaces or one, and only reaching all may last until revoked', () => {
  const lasting = read({grant_kind: 'server_key', grant_expires: 'never'});
  assert.equal(lasting.grantServerKey, true);
  assert.equal(lasting.grantExpiresMs, null);
  assert.equal(requestWords(lasting).until, 'Until you revoke it');
  const chosen = read({grant_kind: 'server_key', grant_expires: String(NOW + 5 * MAX_GRANT_LIFETIME_MS)});
  assert.equal(chosen.grantExpiresMs, NOW + 5 * MAX_GRANT_LIFETIME_MS);
  assert.throws(() => read({grant_kind: 'server_key', grant_expires: String(NOW)}), /has expired/);
  // A key the person makes for one workspace is theirs to approve too; like
  // an agent's grant it ends within the 90 days one may last.
  const one = read({grant_kind: 'server_key', grant_scope: WORKSPACE});
  assert.equal(one.grantServerKey, true);
  assert.equal(one.grantScope, WORKSPACE);
  assert.equal(one.grantExpiresMs, NOW + 7 * DAY);
  assert.throws(() => read({grant_kind: 'server_key', grant_scope: WORKSPACE, grant_expires: 'never'}),
    /grant Tilo cannot give/);
  assert.throws(() => read({grant_kind: 'server_key', grant_scope: WORKSPACE,
    grant_expires: String(NOW + MAX_GRANT_LIFETIME_MS + 1)}), /longer than Tilo gives/);
  assert.throws(() => read({grant_kind: 'agent'}), /grant Tilo cannot give/);
  // Only a server key may last until revoked.
  assert.throws(() => read({grant_expires: 'never'}), /grant Tilo cannot give/);
});

test('a link may name no grant at all: the page then proves the person alone', () => {
  const request = read({grant_key: null, grant_scope: null, grant_expires: null});
  assert.equal(request.grantKey, null);
  assert.equal(request.grantScope, null);
  assert.equal(request.grantExpiresMs, null);
});

test('a grant asked for in part is one the service cannot name', () => {
  for (const missing of ['grant_key', 'grant_scope', 'grant_expires']) {
    assert.throws(() => read({[missing]: null}), /asks for a grant Tilo cannot give/);
  }
});

test('the answer may only go back to the service that asked', () => {
  assert.throws(() => read({return: 'https://elsewhere.example/sign-in'}), /somewhere other than the service/);
  assert.throws(() => read({return: 'https://entry.example/consent'}), /somewhere other than the service/);
  assert.throws(() => read({return: 'https://entry.example'}), /somewhere other than the service/);
});

test('only an origin a sign-in may answer is a service', () => {
  assert.equal(signInOrigin('https://entry.example').origin, 'https://entry.example');
  assert.equal(signInOrigin('https://entry.example:8443/'), null);
  assert.equal(signInOrigin('https://entry.example/path'), null);
  assert.equal(signInOrigin('https://entry.example?q=1'), null);
  assert.equal(signInOrigin('https://user:pw@entry.example'), null);
  assert.equal(signInOrigin('http://entry.example'), null);
  assert.equal(signInOrigin('http://localhost:8787').host, 'localhost:8787');
  assert.equal(signInOrigin('http://127.0.0.1:8787').host, '127.0.0.1:8787');
  assert.equal(signInOrigin('javascript:alert(1)'), null);
  assert.throws(() => read({audience: 'http://entry.example', return: 'http://entry.example/sign-in'}),
    /does not name the service/);
});

test('a challenge, a grant key and a request are what their kinds are', () => {
  assert.throws(() => read({challenge: 'abc'}), /no usable challenge/);
  assert.throws(() => read({challenge: 'zz'.repeat(32)}), /no usable challenge/);
  assert.throws(() => read({challenge: ''}), /no usable challenge/);
  assert.throws(() => read({pending: ''}), /no request from the service/);
  assert.throws(() => read({pending: 'req 1'}), /no request from the service/);
  assert.throws(() => read({grant_key: 'short'}), /asks for a grant Tilo cannot give/);
  assert.throws(() => read({grant_scope: 'someworkspace'}), /workspace Tilo cannot read/);
  assert.throws(() => read({grant_scope: WORKSPACE.toUpperCase()}), /workspace Tilo cannot read/);
  assert.equal(read({grant_scope: WORKSPACE}).grantScope, WORKSPACE);
});

test('a grant is for a while ahead, and no longer than Tilo gives', () => {
  assert.throws(() => read({grant_expires: String(NOW)}), /has expired/);
  assert.throws(() => read({grant_expires: String(NOW - 1)}), /has expired/);
  assert.throws(() => read({grant_expires: '1759881600000.5'}), /asks for a grant Tilo cannot give/);
  assert.throws(() => read({grant_expires: String(NOW + MAX_GRANT_LIFETIME_MS + 1)}), /longer than Tilo gives/);
  assert.equal(read({grant_expires: String(NOW + MAX_GRANT_LIFETIME_MS)}).grantExpiresMs, NOW + MAX_GRANT_LIFETIME_MS);
});

test('a link that names a field twice, or lacks one, is no link of the service', () => {
  assert.throws(() => readSignInRequest(`${link()}&pending=req-2`, {now: NOW}), /not a complete Tilo sign-in link/);
  assert.throws(() => readSignInRequest(link({pending: null}), {now: NOW}), /not a complete Tilo sign-in link/);
  assert.throws(() => readSignInRequest(link({client: null}), {now: NOW}), /not a complete Tilo sign-in link/);
});

test('the card says who asks, through which service, what it reaches and until when', () => {
  assert.deepEqual(requestWords(read(), {locale: 'en-GB'}), {
    heading: 'Let Claude Code act for you?',
    service: 'Through entry.example.',
    reaches: 'All your workspaces, and it may create new ones',
    until: untilText(NOW + 7 * DAY, 'en-GB'),
    pages: 'It asks you before every page action.',
  });
  // A workspace the page knows no name for is said in plain words, never by
  // its id.
  const one = requestWords(read({grant_scope: WORKSPACE}), {locale: 'en-GB'});
  assert.equal(one.reaches, 'Only one of your workspaces');
  const proofOnly = requestWords(read({grant_key: null, grant_scope: null, grant_expires: null}));
  assert.deepEqual(proofOnly, {
    heading: 'Sign in to entry.example?',
    service: 'Tilo in this browser will prove to entry.example that this is your device.',
    reaches: null,
    until: null,
    pages: null,
  });
});

test('a grant reaches a workspace by its name, never by its id', () => {
  const one = requestWords(read({grant_scope: WORKSPACE}), {
    locale: 'en', home: {workspace: WORKSPACE, name: 'Lounge'},
  });
  assert.equal(one.reaches, 'Only the workspace “Lounge”');
  assert.doesNotMatch(one.reaches, new RegExp(WORKSPACE));
  // The page's site names its home: entry.json's admission address spells the
  // workspace, and home.name is the name people read it by.
  assert.deepEqual(siteHome({home: {name: 'Lounge', admission: `https://eu1.tilo.example/peer/v1/directory/${WORKSPACE}/`}}),
    {workspace: WORKSPACE, name: 'Lounge'});
  assert.equal(siteHome({home: {name: 'Lounge'}}), null);
  assert.equal(siteHome({home: {admission: 'https://eu1.tilo.example/peer/v1/directory/../../etc/'}}), null);
});

test('a grant whose workspace the page cannot name says so plainly', () => {
  for (const words of [requestWords(read({grant_scope: WORKSPACE})),
    requestWords(read({grant_scope: WORKSPACE}), {home: {workspace: 'another', name: 'Studio'}})]) {
    assert.equal(words.reaches, 'Only one of your workspaces');
    assert.doesNotMatch(words.reaches, new RegExp(WORKSPACE));
  }
});

test('a service on this computer is said in plain words, not its address', () => {
  const local = {audience: 'http://localhost:24610', return: 'http://localhost:24610/sign-in'};
  assert.equal(requestWords(read(local)).service, 'From a terminal on this computer.');
  assert.deepEqual(serviceWords('http://127.0.0.1:8787'), {name: null, local: true});
  assert.equal(requestWords(read({...local, audience: 'http://127.0.0.1:8787', return: 'http://127.0.0.1:8787/sign-in'})).service,
    'From a terminal on this computer.');
  // A site people know says where the request comes from.
  assert.equal(requestWords(read()).service, 'Through entry.example.');
  // An address that names no site says nothing at all.
  const address = {audience: 'https://203.0.113.7', return: 'https://203.0.113.7/sign-in'};
  assert.equal(requestWords(read(address)).service, null);
  assert.deepEqual(serviceWords('https://203.0.113.7'), {name: null, local: false});
});

test('the date reads in the page’s language, not the browser’s', () => {
  const english = requestWords(read(), {locale: 'en'});
  assert.equal(english.until, untilText(NOW + 7 * DAY, 'en'));
  // The page is English whatever the browser was set to: no German month on it.
  assert.doesNotMatch(english.until, /\b(?:Januar|Februar|März|April|Mai|Juni|Juli|August|September|Oktober|November|Dezember)\b/u);
  assert.equal(pageLocale({documentElement: {lang: 'en'}}), 'en');
  assert.equal(pageLocale({documentElement: {lang: 'de-DE'}}), 'de-DE');
  assert.equal(pageLocale({documentElement: {}}), 'en');
  assert.equal(pageLocale({documentElement: {lang: 'not a tag'}}), 'en');
  // A German page would date it in German: the locale is the page's, not fixed.
  assert.equal(requestWords(read(), {locale: 'de-DE'}).until, untilText(NOW + 7 * DAY, 'de-DE'));
});

test('a client name is the service’s text, never its markup', () => {
  const words = requestWords(read({client: '<img src=x onerror=alert(1)>'}));
  assert.equal(words.heading, 'Let <img src=x onerror=alert(1)> act for you?');
  const control = read({client: 'Claude\nCode'});
  assert.equal(control.client, 'ClaudeCode');
});

test('the answer is a form field the service resolves its sign-in from', () => {
  const request = read();
  assert.deepEqual([...allowFields(request, {proof: 'proof-text', grant: 'grant-text'})], [
    ['pending', 'req-1'], ['proof', 'proof-text'], ['grant', 'grant-text'],
  ]);
  assert.deepEqual([...allowFields(request, {proof: 'proof-text'})], [
    ['pending', 'req-1'], ['proof', 'proof-text'],
  ]);
  assert.deepEqual([...denyFields(request)], [['pending', 'req-1'], ['decision', 'deny']]);
});

class Element {
  constructor(tag = 'div') {
    this.tag = tag;
    this.children = [];
    this.listeners = new Map();
    this.disabled = false;
    this.hidden = false;
    this.className = '';
    this.name = '';
    this.value = '';
    this.checked = false;
    this.textContent = '';
  }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  async fire(type) {
    await (this.listeners.get(type) ?? this[`on${type}`])?.({preventDefault() {}});
    for (let i = 0; i < 40; i++) await Promise.resolve();
  }
}

const NAMES = ['heading', 'request', 'service', 'reaches', 'reachesRow', 'scope', 'scopeRow', 'until', 'untilRow',
  'pages', 'pagesRow', 'message', 'actions', 'allow', 'cancel', 'signedOut', 'signedOutText', 'passkey', 'start', 'copy'];

/** A store with the workspaces `entries` as this browser entered them, as
 * `rememberEntered` writes them. */
function entered(...entries) {
  const items = new Map();
  if (entries.length) {
    items.set('tilo-entered-workspaces', JSON.stringify(entries.map(([workspace, name]) => ({
      origin: 'https://eu1.tilo.example', workspace, hostDevice: 'a'.repeat(64), ...(name ? {name} : {}),
    }))));
  }
  return {getItem: key => items.get(key) ?? null, setItem: (key, value) => items.set(key, value), removeItem: key => items.delete(key)};
}

/** A sign-in page at `search`, its card's elements, and the forms it posts. */
function page({
  search = link(), kept = {rootPublicKey: ROOT, held: 'browser'}, local = entered(),
  identity = {userPublicKey: ROOT, locked: false, sign: async () => new Uint8Array(64)},
  passkeys = true, signIn = async () => { throw Object.assign(new Error('no'), {code: 'passkey-cancelled'}); },
  ownDeviceKeyStore = false, siteEntry = null, documentLang = 'en',
} = {}) {
  const store = {kept};
  // The person the store keeps, as the one store writes them: the identity
  // store's record beside the tab's key, or nothing when there is nobody.
  const personJson = () => JSON.stringify(personRecord({seed: new Uint8Array(32), publicKey: unhex(ROOT)}));
  const elements = Object.fromEntries(NAMES.map(name => [name, new Element()]));
  const posted = [];
  const formed = [];
  const document = {
    documentElement: {dataset: {}, lang: documentLang},
    body: {append() {}},
    createElement(tag) {
      const element = new Element(tag);
      if (tag !== 'form') return element;
      Object.assign(element, {
        method: '', action: '', acceptCharset: '', submitted: 0,
        submit() { this.submitted += 1; },
      });
      formed.push(element);
      return element;
    },
  };
  const listeners = new Map();
  const window = {
    document, location: {search, href: `https://web.example/sign-in${search}`},
    fetch: siteEntry ? async () => ({ok: true, json: async () => siteEntry}) : undefined,
    PublicKeyCredential: passkeys ? function PublicKeyCredential() {} : undefined,
    navigator: {credentials: passkeys ? {get() {}} : undefined},
    addEventListener: (type, listener) => listeners.set(type, listener),
    removeEventListener: type => listeners.delete(type),
    focus: async () => { await listeners.get('focus')?.(); for (let i = 0; i < 40; i++) await Promise.resolve(); },
  };
  const runtime = runtimeFake({
    async signInAnswer(person, signer, ask) {
      const asked = JSON.parse(ask);
      posted.push({person, signer, ask: asked});
      // What the runtime answers: a grant only where the link asked for one.
      return JSON.stringify(asked.grantKey ? {proof: 'proof-text', grant: 'grant-text'} : {proof: 'proof-text'});
    },
  });
  const mounted = startSignInPage({
    window,
    elements,
    storage: {get: async key => (key === PERSON_KEY && store.kept ? personJson() : undefined), put: async () => {}},
    signIn: async options => { await signIn(options); store.kept = {rootPublicKey: ROOT, held: 'browser'}; },
    hosts: async () => ['https://eu1.tilo.example'],
    local,
    identity,
    loadRuntime: async () => runtime,
    // Handed nothing, as sign-in.html hands it nothing, the page takes the store it is built with.
    openDeviceKey: ownDeviceKeyStore ? undefined : async request => {
      posted.push({deviceKey: request});
      return {kind: 'webcrypto-ed25519', close: async () => {}};
    },
  });
  /** The choices the card offers for what the grant reaches, as the person reads them. */
  const scopes = () => (elements.scopeRow.hidden ? null
    : elements.scope.children.map(option => ({
      name: option.children[1].textContent,
      value: option.children[0].value,
      checked: option.children[0].checked,
    })));
  return {elements, document, window, store, formed, posted, mounted, scopes, shown: () => ({
    state: document.documentElement.dataset.signIn,
    heading: elements.heading.textContent,
    service: elements.service.textContent,
    reaches: elements.reachesRow.hidden ? '' : elements.reaches.textContent,
    until: elements.untilRow.hidden ? '' : elements.until.textContent,
    pages: elements.pagesRow.hidden ? '' : elements.pages.textContent,
    actions: elements.actions.hidden ? 'none' : 'offered',
    message: elements.message.textContent,
  })};
}

const link_ = (over) => link(over);
/** `grantExpiresMs` in the words the card shows it in. */
const untilText = (expires, locale = 'en') =>
  new Date(expires).toLocaleDateString(locale, {year: 'numeric', month: 'long', day: 'numeric'});

test('the page starts when nothing hands it a device key store', async () => {
  const {mounted, shown} = page({ownDeviceKeyStore: true});
  await assert.doesNotReject(mounted);
  assert.equal(shown().actions, 'offered');
});

test('allow signs what the link asked for and posts it to the service', async () => {
  const {elements, formed, posted, shown, mounted} = page({search: link_()});
  await mounted;
  await elements.allow.fire('click');
  assert.equal(shown().state, 'asked');
  assert.equal(formed.length, 1);
  const [form] = formed;
  assert.equal(form.method, 'POST');
  assert.equal(form.action, 'https://entry.example/sign-in');
  assert.equal(form.acceptCharset, 'utf-8');
  assert.deepEqual(form.children.map(input => [input.type, input.name, input.value]), [
    ['hidden', 'pending', 'req-1'], ['hidden', 'proof', 'proof-text'], ['hidden', 'grant', 'grant-text'],
  ]);
  assert.equal(form.submitted, 1);
  const ask = posted.find(entry => entry.ask)?.ask;
  assert.deepEqual(ask, {
    audience: 'https://entry.example',
    challenge: CHALLENGE,
    client: 'Claude Code',
    grantKey: KEY,
    grantScope: 'all',
    expiresAtMs: NOW + 7 * DAY,
    serverKey: false,
    operateBrowserPages: false,
  });
  const deviceKey = posted.find(entry => entry.deviceKey)?.deviceKey;
  assert.equal(deviceKey.profile, `tilo-identity:${ROOT}@entry.example`);
});

test('a link that asks for no grant proves the person and posts no grant', async () => {
  const {elements, formed, posted, mounted} = page({
    search: link_({grant_key: null, grant_scope: null, grant_expires: null}),
  });
  await mounted;
  await elements.allow.fire('click');
  assert.deepEqual(formed[0].children.map(input => input.name), ['pending', 'proof']);
  assert.equal(posted.find(entry => entry.ask)?.ask.grantKey, undefined);
});

test('cancel answers the service with a denial and signs nothing', async () => {
  const {elements, formed, posted, shown, mounted} = page({search: link_()});
  await mounted;
  assert.equal(shown().actions, 'offered');
  await elements.cancel.fire('click');
  assert.equal(posted.length, 0);
  assert.deepEqual(formed[0].children.map(input => [input.name, input.value]), [
    ['pending', 'req-1'], ['decision', 'deny'],
  ]);
  assert.equal(formed[0].action, 'https://entry.example/sign-in');
  assert.equal(formed[0].submitted, 1);
});

test('nothing is signed before the person decides', async () => {
  const {formed, posted, shown, mounted} = page({search: link_()});
  await mounted;
  assert.equal(posted.length, 0);
  assert.equal(formed.length, 0);
  assert.equal(shown().heading, 'Let Claude Code act for you?');
  assert.equal(shown().service, 'Through entry.example.');
  assert.equal(shown().reaches, 'All your workspaces, and it may create new ones');
  assert.equal(shown().until, untilText(NOW + 7 * DAY));
});

const STUDIO = '0b0c8f0e-6f4a-4f2f-9d1c-2f2b0a5e11aa';
const LOUNGE = '1c2d3e4f-5a6b-4c8d-9e0f-1a2b3c4d5e6f';

test('a service that asks for every workspace lets the person hold the agent to one', async () => {
  const {elements, formed, posted, scopes, shown, mounted} = page({
    search: link_(), local: entered([LOUNGE, 'Lounge'], [STUDIO, 'Studio']),
  });
  await mounted;
  // The service's own ask first, checked; then the workspaces by name.
  assert.deepEqual(scopes(), [
    {name: 'All my workspaces', value: 'all', checked: true},
    {name: 'Lounge', value: LOUNGE, checked: false},
    {name: 'Studio', value: STUDIO, checked: false},
  ]);
  // What the grant reaches is the choice itself, not a second line saying it.
  assert.equal(shown().reaches, '');
  // Allowed as it stands, the grant reaches all of them.
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, 'all');
  assert.deepEqual(formed[0].children.map(input => input.name), ['pending', 'proof', 'grant']);
});

test('the workspace the person chooses is the one the grant reaches', async () => {
  const {elements, posted, mounted} = page({
    search: link_(), local: entered([LOUNGE, 'Lounge'], [STUDIO, 'Studio']),
  });
  await mounted;
  const studio = elements.scope.children[2].children[0];
  studio.checked = true;
  await studio.fire('change');
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, STUDIO);
});

test('a service that asks for one workspace is held to it, and the card names it', async () => {
  const {elements, posted, scopes, shown, mounted} = page({
    search: link_({grant_scope: STUDIO}), local: entered([LOUNGE, 'Lounge'], [STUDIO, 'Studio']),
  });
  await mounted;
  assert.equal(shown().reaches, 'Only the workspace “Studio”');
  // Nothing to choose: the service asked for that one.
  assert.equal(scopes(), null);
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, STUDIO);
});

test('a card nobody is signed in to says which workspace the key reaches', async () => {
  const {shown, mounted} = page({
    search: link_({grant_kind: 'server_key', grant_scope: STUDIO}),
    local: entered([LOUNGE, 'Lounge'], [STUDIO, 'Studio']), kept: null,
  });
  await mounted;
  assert.equal(shown().state, 'signed-out');
  assert.equal(shown().reaches, 'Only the workspace “Studio”');
});

test('an API key the person makes for one workspace is approved as that server key', async () => {
  const {elements, posted, scopes, shown, mounted} = page({
    search: link_({grant_kind: 'server_key', grant_scope: STUDIO, client: 'API key “Visa backend”'}),
    local: entered([LOUNGE, 'Lounge'], [STUDIO, 'Studio']),
  });
  await mounted;
  assert.equal(shown().heading, 'Let API key “Visa backend” act for you?');
  assert.equal(shown().reaches, 'Only the workspace “Studio”');
  assert.equal(scopes(), null);
  await elements.allow.fire('click');
  assert.deepEqual(posted.find(entry => entry.ask)?.ask, {
    audience: 'https://entry.example',
    challenge: CHALLENGE,
    client: 'API key “Visa backend”',
    grantKey: KEY,
    grantScope: STUDIO,
    expiresAtMs: NOW + 7 * DAY,
    serverKey: true,
    operateBrowserPages: false,
  });
});

test('the card names the site’s home workspace a grant reaches', async () => {
  const {elements, shown, mounted} = page({
    search: link_({grant_scope: LOUNGE}),
    siteEntry: {home: {name: 'Lounge', admission: `https://eu1.tilo.example/peer/v1/directory/${LOUNGE}/`}},
  });
  await mounted;
  assert.equal(shown().state, 'asked');
  assert.equal(shown().reaches, 'Only the workspace “Lounge”');
  assert.doesNotMatch(elements.reaches.textContent, new RegExp(LOUNGE));
});

test('a workspace the page cannot name is said plainly, never by its id', async () => {
  for (const siteEntry of [null,
    {home: {name: 'Studio', admission: `https://eu1.tilo.example/peer/v1/directory/${STUDIO}/`}}]) {
    const {elements, shown, mounted} = page({search: link_({grant_scope: LOUNGE}), siteEntry});
    await mounted;
    assert.equal(shown().reaches, 'Only one of your workspaces');
    assert.doesNotMatch(elements.reaches.textContent, new RegExp(LOUNGE));
  }
});

test('a service on this computer is said plainly on the card, not by its port', async () => {
  const {elements, shown, mounted} = page({
    search: link_({audience: 'http://localhost:24610', return: 'http://localhost:24610/sign-in'}),
  });
  await mounted;
  assert.equal(shown().service, 'From a terminal on this computer.');
  assert.equal(elements.service.hidden, false);
  assert.doesNotMatch(elements.service.textContent, /localhost|24610/);
});

test('a card whose service is no name a person knows drops the line', async () => {
  const {elements, shown, mounted} = page({
    search: link_({audience: 'https://203.0.113.7', return: 'https://203.0.113.7/sign-in'}),
  });
  await mounted;
  assert.equal(shown().service, '');
  assert.equal(elements.service.hidden, true);
});

test('the card’s date is in the page’s language, not the browser’s', async () => {
  const {shown, mounted} = page({search: link_({grant_scope: LOUNGE}), documentLang: 'en'});
  await mounted;
  assert.equal(shown().until, untilText(NOW + 7 * DAY, 'en'));
  assert.doesNotMatch(shown().until, /\b(?:Januar|Februar|März|Mai|Juni|Juli|Oktober|Dezember)\b|\d{1,2}\.\s+\p{Lu}/u);
});

test('a browser that knows no workspaces is offered no choice', async () => {
  const {elements, posted, scopes, shown, mounted} = page({search: link_(), local: entered()});
  await mounted;
  assert.equal(scopes(), null);
  assert.equal(shown().reaches, 'All your workspaces, and it may create new ones');
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, 'all');
});

test('a person this browser keeps but cannot open is told to reload, and nothing is signed', async () => {
  const {elements, formed, posted, shown, mounted} = page({
    search: link_(),
    identity: {userPublicKey: ROOT, locked: true},
  });
  await mounted;
  await elements.allow.fire('click');
  assert.equal(posted.length, 0);
  assert.equal(formed.length, 0);
  assert.match(shown().message, /Reload it to answer/);
  assert.equal(shown().actions, 'offered');
});

test('a browser with nobody signed in asks the question and offers the ways in on the card', async () => {
  const {elements, formed, shown, mounted} = page({search: link_(), kept: null});
  await mounted;
  assert.equal(shown().state, 'signed-out');
  assert.match(shown().heading, /Let Claude Code act for you/);
  // What it reaches is said before the person signs in to answer it.
  assert.equal(shown().reaches, 'All your workspaces, and it may create new ones');
  assert.equal(elements.signedOut.hidden, false);
  assert.match(elements.signedOutText.textContent, /Sign in to Tilo in this browser/);
  assert.equal(elements.passkey.hidden, false);
  assert.equal(elements.start.href, 'https://web.example/');
  assert.equal(shown().actions, 'none');
  await elements.allow.fire('click');
  assert.equal(formed.length, 0);
});

test('signing in with a passkey on the card goes on to the question', async () => {
  const used = [];
  const {elements, formed, shown, mounted} = page({search: link_(), kept: null, signIn: async options => used.push(options)});
  await mounted;
  await elements.passkey.fire('click');
  assert.equal(used.length, 1);
  assert.equal(typeof used[0].fetchEnvelope, 'function');
  assert.equal(shown().state, 'asked');
  assert.equal(elements.signedOut.hidden, true);
  assert.equal(shown().actions, 'offered');
  await elements.allow.fire('click');
  assert.equal(formed.length, 1);
});

test('a passkey that opens no identity says so and keeps the ways in', async () => {
  const {elements, shown, mounted} = page({search: link_(), kept: null,
    signIn: async () => { throw Object.assign(new Error('none'), {code: 'no-envelope'}); }});
  await mounted;
  await elements.passkey.fire('click');
  assert.equal(shown().state, 'signed-out');
  assert.match(shown().message, /doesn’t open a Tilo identity/);
  assert.equal(elements.passkey.disabled, false);
});

test('a place that cannot use passkeys says where the person can answer instead', async () => {
  // A web view inside an app that may not use this site's passkeys: refused before anything
  // was shown, which no second try changes.
  const {elements, shown, mounted} = page({search: link_(), kept: null,
    signIn: async () => { throw Object.assign(new Error('Passkeys cannot be used here'), {code: 'passkeys-refused'}); }});
  await mounted;
  await elements.passkey.fire('click');
  assert.equal(shown().state, 'signed-out');
  assert.equal(shown().message, 'Passkeys can’t be used here. Open this page in the browser where you use Tilo.');
  assert.equal(elements.passkey.disabled, false);
});

test('signing in from another tab brings the card on to the question', async () => {
  const {window, store, shown, mounted} = page({search: link_(), kept: null});
  await mounted;
  store.kept = {rootPublicKey: ROOT, held: 'browser'};
  await window.focus();
  assert.equal(shown().state, 'asked');
  assert.equal(shown().actions, 'offered');
});

test('a browser without passkeys offers Tilo itself', async () => {
  const {elements, mounted} = page({search: link_(), kept: null, passkeys: false});
  await mounted;
  assert.equal(elements.passkey.hidden, true);
  assert.equal(elements.start.textContent, 'Open Tilo');
});

test('a link the route will not answer shows why, and asks nothing', async () => {
  const {elements, shown, mounted} = page({search: link_({return: 'https://elsewhere.example/sign-in'})});
  await mounted;
  assert.equal(shown().state, 'stopped');
  assert.match(shown().heading, /somewhere other than the service/);
  assert.equal(shown().service, '');
  assert.equal(shown().actions, 'none');
  await elements.allow.fire('click');
  assert.equal(shown().message, '');
});

test('the route’s page is the card the module drives', () => {
  const html = readFileSync(new URL('./sign-in.html', import.meta.url), 'utf8');
  for (const id of ['sign-in-heading', 'sign-in-request', 'sign-in-service', 'sign-in-reaches',
    'sign-in-reaches-row', 'sign-in-scope', 'sign-in-scope-row', 'sign-in-until', 'sign-in-until-row',
    'sign-in-message', 'sign-in-actions', 'sign-in-allow', 'sign-in-cancel', 'sign-in-signed-out',
    'sign-in-signed-out-text', 'sign-in-passkey', 'sign-in-start', 'sign-in-copy']) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} is missing`);
  }
  // The page starts this module, which layout.py names in the versioned
  // directory, and preloads it.
  assert.match(html, /import \{startSignInPage\} from '\.\/sign-in\.mjs';/);
  assert.match(html, /<link rel="modulepreload" href="\.\/sign-in\.mjs">/);
});
