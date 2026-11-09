import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  allowFields, denyFields, MAX_GRANT_LIFETIME_MS, readSignInRequest, requestWords, signInOrigin, startSignInPage,
} from './sign-in.mjs';

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
  });
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
  });
  const one = requestWords(read({grant_scope: WORKSPACE}), {locale: 'en-GB'});
  assert.equal(one.reaches, `Only the workspace ${WORKSPACE}`);
  const proofOnly = requestWords(read({grant_key: null, grant_scope: null, grant_expires: null}));
  assert.deepEqual(proofOnly, {
    heading: 'Sign in to door.example?',
    service: 'Tilo in this browser will prove to door.example that this is your device.',
    reaches: null,
    until: null,
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
  constructor() {
    this.children = [];
    this.listeners = new Map();
    this.disabled = false;
    this.hidden = false;
    this.textContent = '';
  }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  async fire(type) {
    await this.listeners.get(type)?.({preventDefault() {}});
    for (let i = 0; i < 40; i++) await Promise.resolve();
  }
}

const NAMES = ['heading', 'request', 'service', 'reaches', 'reachesRow', 'until', 'untilRow', 'message',
  'actions', 'allow', 'cancel'];

/** A sign-in page at `search`, its card's elements, and the forms it posts. */
function page({search = link(), kept = {rootPublicKey: ROOT, held: 'browser'}, identity = {
  userPublicKey: ROOT, locked: false, sign: async () => new Uint8Array(64),
}} = {}) {
  const elements = Object.fromEntries(NAMES.map(name => [name, new Element()]));
  const posted = [];
  const formed = [];
  const document = {
    documentElement: {dataset: {}},
    body: {append() {}},
    createElement(tag) {
      if (tag !== 'form') return {};
      const form = {
        method: '', action: '', acceptCharset: '', hidden: false, children: [], submitted: 0,
        append(child) { this.children.push(child); },
        submit() { this.submitted += 1; },
      };
      formed.push(form);
      return form;
    },
  };
  const window = {document, location: {search}};
  const runtime = {
    async signInAnswer(person, signer, ask) {
      const asked = JSON.parse(ask);
      posted.push({person, signer, ask: asked});
      // What the runtime answers: a grant only where the link asked for one.
      return JSON.stringify(asked.grantKey ? {proof: 'proof-text', grant: 'grant-text'} : {proof: 'proof-text'});
    },
  };
  const mounted = startSignInPage({
    window,
    elements,
    storage: {get: async () => kept, put: async () => {}},
    identity,
    loadRuntime: async () => runtime,
    openCustody: async request => {
      posted.push({custody: request});
      return {kind: 'webcrypto-ed25519', close: async () => {}};
    },
  });
  return {elements, document, formed, posted, mounted, shown: () => ({
    state: document.documentElement.dataset.signIn,
    heading: elements.heading.textContent,
    service: elements.service.textContent,
    reaches: elements.reachesRow.hidden ? '' : elements.reaches.textContent,
    until: elements.untilRow.hidden ? '' : elements.until.textContent,
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
    'sign-in-reaches-row', 'sign-in-until', 'sign-in-until-row', 'sign-in-message', 'sign-in-actions',
    'sign-in-allow', 'sign-in-cancel']) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} is missing`);
  }
  // The page starts this module, which layout.py names in the versioned
  // directory, and preloads it.
  assert.match(html, /import \{startSignInPage\} from '\.\/sign-in\.mjs';/);
  assert.match(html, /<link rel="modulepreload" href="\.\/sign-in\.mjs">/);
});
