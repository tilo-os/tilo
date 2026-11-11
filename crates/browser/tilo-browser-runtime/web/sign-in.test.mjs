import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  allowFields, denyFields, MAX_GRANT_LIFETIME_MS, readSignInRequest, requestWords, signInOrigin, startSignInPage,
} from './sign-in.mjs';
import {runtimeFake} from './runtime-surface.mjs';

const NOW = 1_800_000_000_000;
const CHALLENGE = 'a'.repeat(64);
const KEY = 'b'.repeat(64);
const WORKSPACE = '3f2a1c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d';
const ROOT = 'c'.repeat(64);
const DAY = 24 * 60 * 60 * 1000;

/** The link a service writes for the person's browser, as given. */
function link(over = {}) {
  const params = {
    audience: 'https://door.example',
    challenge: CHALLENGE,
    return: 'https://door.example/sign-in',
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
    audience: 'https://door.example',
    host: 'door.example',
    challenge: CHALLENGE,
    returnTo: 'https://door.example/sign-in',
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

test('a server key reaches all workspaces and lasts until revoked, or as long as the person chose', () => {
  const lasting = read({grant_kind: 'server_key', grant_expires: 'never'});
  assert.equal(lasting.grantServerKey, true);
  assert.equal(lasting.grantExpiresMs, null);
  assert.equal(requestWords(lasting).until, 'Until you revoke it');
  const chosen = read({grant_kind: 'server_key', grant_expires: String(NOW + 5 * MAX_GRANT_LIFETIME_MS)});
  assert.equal(chosen.grantExpiresMs, NOW + 5 * MAX_GRANT_LIFETIME_MS);
  assert.throws(() => read({grant_kind: 'server_key', grant_expires: String(NOW)}), /has expired/);
  assert.throws(() => read({grant_kind: 'server_key', grant_scope: WORKSPACE}), /grant Tilo cannot give/);
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
  assert.throws(() => read({return: 'https://door.example/consent'}), /somewhere other than the service/);
  assert.throws(() => read({return: 'https://door.example'}), /somewhere other than the service/);
});

test('only an origin a sign-in may answer is a service', () => {
  assert.equal(signInOrigin('https://door.example').origin, 'https://door.example');
  assert.equal(signInOrigin('https://door.example:8443/'), null);
  assert.equal(signInOrigin('https://door.example/path'), null);
  assert.equal(signInOrigin('https://door.example?q=1'), null);
  assert.equal(signInOrigin('https://user:pw@door.example'), null);
  assert.equal(signInOrigin('http://door.example'), null);
  assert.equal(signInOrigin('http://localhost:8787').host, 'localhost:8787');
  assert.equal(signInOrigin('http://127.0.0.1:8787').host, '127.0.0.1:8787');
  assert.equal(signInOrigin('javascript:alert(1)'), null);
  assert.throws(() => read({audience: 'http://door.example', return: 'http://door.example/sign-in'}),
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
    service: 'Through door.example.',
    reaches: 'All your workspaces, and it may create new ones',
    until: untilText(NOW + 7 * DAY, 'en-GB'),
    pages: 'It asks you before every page action.',
  });
  const one = requestWords(read({grant_scope: WORKSPACE}), {locale: 'en-GB'});
  assert.equal(one.reaches, `Only the workspace ${WORKSPACE}`);
  const proofOnly = requestWords(read({grant_key: null, grant_scope: null, grant_expires: null}));
  assert.deepEqual(proofOnly, {
    heading: 'Sign in to door.example?',
    service: 'Tilo in this browser will prove to door.example that this is your device.',
    reaches: null,
    until: null,
    pages: null,
  });
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
    await this.listeners.get(type)?.({preventDefault() {}});
    for (let i = 0; i < 40; i++) await Promise.resolve();
  }
}

const NAMES = ['heading', 'request', 'service', 'reaches', 'reachesRow', 'scope', 'scopeRow', 'until', 'untilRow',
  'pages', 'pagesRow', 'message', 'actions', 'allow', 'cancel'];

/** A store with the workspaces `entries` as this browser entered them, as
 * `rememberEntered` writes them. */
function entered(...entries) {
  const items = new Map();
  if (entries.length) {
    items.set('tilo-entered-workspaces', JSON.stringify(entries.map(([room, name]) => ({
      origin: 'https://eu1.tilo.example', room, hostDevice: 'a'.repeat(64), ...(name ? {name} : {}),
    }))));
  }
  return {getItem: key => items.get(key) ?? null, setItem: (key, value) => items.set(key, value), removeItem: key => items.delete(key)};
}

/** A sign-in page at `search`, its card's elements, and the forms it posts. */
function page({
  search = link(), kept = {rootPublicKey: ROOT, held: 'browser'}, local = entered(),
  identity = {userPublicKey: ROOT, locked: false, sign: async () => new Uint8Array(64)},
} = {}) {
  const elements = Object.fromEntries(NAMES.map(name => [name, new Element()]));
  const posted = [];
  const formed = [];
  const document = {
    documentElement: {dataset: {}},
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
  const window = {document, location: {search}};
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
    storage: {get: async () => kept, put: async () => {}},
    local,
    identity,
    loadRuntime: async () => runtime,
    openCustody: async request => {
      posted.push({custody: request});
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
  return {elements, document, formed, posted, mounted, scopes, shown: () => ({
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
const untilText = (expires, locale = undefined) =>
  new Date(expires).toLocaleDateString(locale, {year: 'numeric', month: 'long', day: 'numeric'});

test('allow signs what the link asked for and posts it to the service', async () => {
  const {elements, formed, posted, shown, mounted} = page({search: link_()});
  await mounted;
  await elements.allow.fire('click');
  assert.equal(shown().state, 'asked');
  assert.equal(formed.length, 1);
  const [form] = formed;
  assert.equal(form.method, 'POST');
  assert.equal(form.action, 'https://door.example/sign-in');
  assert.equal(form.acceptCharset, 'utf-8');
  assert.deepEqual(form.children.map(input => [input.type, input.name, input.value]), [
    ['hidden', 'pending', 'req-1'], ['hidden', 'proof', 'proof-text'], ['hidden', 'grant', 'grant-text'],
  ]);
  assert.equal(form.submitted, 1);
  const ask = posted.find(entry => entry.ask)?.ask;
  assert.deepEqual(ask, {
    audience: 'https://door.example',
    challenge: CHALLENGE,
    client: 'Claude Code',
    grantKey: KEY,
    grantScope: 'all',
    expiresAtMs: NOW + 7 * DAY,
    serverKey: false,
    operateBrowserPages: false,
  });
  const custody = posted.find(entry => entry.custody)?.custody;
  assert.equal(custody.profile, `tilo-identity:${ROOT}@door.example`);
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
  assert.equal(formed[0].action, 'https://door.example/sign-in');
  assert.equal(formed[0].submitted, 1);
});

test('nothing is signed before the person decides', async () => {
  const {formed, posted, shown, mounted} = page({search: link_()});
  await mounted;
  assert.equal(posted.length, 0);
  assert.equal(formed.length, 0);
  assert.equal(shown().heading, 'Let Claude Code act for you?');
  assert.equal(shown().service, 'Through door.example.');
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
  assert.equal(shown().reaches, 'Only the workspace Studio');
  // Nothing to choose: the service asked for that one.
  assert.equal(scopes(), null);
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, STUDIO);
});

test('a browser that knows no workspaces is offered no choice', async () => {
  const {elements, posted, scopes, shown, mounted} = page({search: link_(), local: entered()});
  await mounted;
  assert.equal(scopes(), null);
  assert.equal(shown().reaches, 'All your workspaces, and it may create new ones');
  await elements.allow.fire('click');
  assert.equal(posted.find(entry => entry.ask)?.ask.grantScope, 'all');
});

test('a person this browser keeps but has not unlocked is told so, and nothing is signed', async () => {
  const {elements, formed, posted, shown, mounted} = page({
    search: link_(),
    identity: {userPublicKey: ROOT, locked: true},
  });
  await mounted;
  await elements.allow.fire('click');
  assert.equal(posted.length, 0);
  assert.equal(formed.length, 0);
  assert.match(shown().message, /Open Tilo in this browser and sign in first/);
  assert.equal(shown().actions, 'offered');
});

test('a browser with no person says how to sign in, and offers nothing to do', async () => {
  const {elements, formed, shown, mounted} = page({search: link_(), kept: null});
  await mounted;
  assert.equal(shown().state, 'stopped');
  assert.match(shown().heading, /Open Tilo in this browser and sign in first/);
  assert.equal(shown().actions, 'none');
  assert.equal(formed.length, 0);
  await elements.allow.fire('click');
  assert.equal(formed.length, 0);
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
    'sign-in-message', 'sign-in-actions', 'sign-in-allow', 'sign-in-cancel']) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} is missing`);
  }
  // The page starts this module, which layout.py names in the versioned
  // directory, and preloads it.
  assert.match(html, /import \{startSignInPage\} from '\.\/sign-in\.mjs';/);
  assert.match(html, /<link rel="modulepreload" href="\.\/sign-in\.mjs">/);
});
