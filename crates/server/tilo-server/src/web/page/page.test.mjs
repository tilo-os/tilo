// The page's handshake, byte for byte as the server's types encode it
// (`handshake.json`, which `tests.rs` checks against those types).
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {ack2, approvalPrompt, authenticatedPage, blake3Block, hello, pagePerson, parseExact, sameSecretEntry, serverNonce, workspaceItemPath} from './page.mjs';

const golden = JSON.parse(readFileSync(new URL('./handshake.json', import.meta.url), 'utf8'));
const bytes = text => Uint8Array.from(text.match(/../g) ?? [], pair => parseInt(pair, 16));
const hex = data => Array.from(data, b => b.toString(16).padStart(2, '0')).join('');

test('a workspace resource opens in the addressed app on the same workspace', () => {
  const item = 'workspace/agents/child 1';
  const path = workspaceItemPath('team-1', 'so.tilo.chat', item);
  assert.equal(path, '/w/team-1/chat/workspace%2Fagents%2Fchild%201');
  assert.equal(decodeURIComponent(new URL(path, 'https://tilo.sh').pathname.split('/')[4]), item);
  assert.equal(workspaceItemPath('team-1', 'so.tilo.assistant'), '/w/team-1/assistant');
  assert.equal(workspaceItemPath('team-1', 'so.tilo.chat', '..'), null);
});

test('only the same password field may keep local typing when a patch arrives', () => {
  const field = (type, target, label) => ({type, dataset: {t: target}, getAttribute: () => label});
  const key = field('password', 'tilo.key-action:secret-prompt:100', 'Provider key');
  assert.ok(sameSecretEntry(key, {...key}));
  assert.ok(!sameSecretEntry(key, {...key, type: 'text'}));
  assert.ok(!sameSecretEntry(key, field('password', 'tilo.key-action:secret-prompt:101', 'Provider key')));
  assert.ok(!sameSecretEntry(key, field('password', key.dataset.t, 'Account password')));
});

test('reconnecting cannot send an old password field to the newly signed device', async () => {
  const page = {
    field: null,
    set innerHTML(body) { this.field = {body, id: 12}; },
    replaceChildren() { this.field = null; },
    contains(field) { return Boolean(field) && field === this.field; },
  };
  const first = [];
  const second = [];
  const connection = sent => ({send: async ask => { sent.push(ask); }});
  const view = authenticatedPage(page);
  const act = {act: {id: 12, target: 'password', action: {value: 'test-only'}}};
  const open = {open: {workspace: 'team', app: 'settings'}};
  view.connect(connection(first));
  view.show('Password');
  const previous = page.field;
  await view.send(act, previous);
  view.connect(null);
  assert.equal(page.field, null);
  assert.equal(view.owns(previous), false);
  view.connect(connection(second));
  await view.send(open);
  await view.send(act, previous);
  await view.send(act);
  assert.deepEqual(second, [open]);
  view.show('Password');
  assert.equal(page.field.id, previous.id);
  await view.send(act, previous);
  assert.deepEqual(second, [open]);
  const current = {act: {id: 12, target: 'password', action: {value: 'new-test-only'}}};
  await view.send(current, page.field);
  assert.deepEqual(second, [open, current]);
  assert.deepEqual(first, [act]);
});

test('only a personal page device may start pairing', () => {
  const person = {user_public_key: [0xab], capabilities: {}};
  assert.equal(pagePerson(person), 'ab');
  for (const capabilities of [{agent: 'all'}, {stand_in: true}, {workspace_service: true}]) {
    assert.equal(pagePerson({...person, capabilities}), null);
  }
  assert.equal(pagePerson({...person, issuer: {...person, capabilities: {workspace_service: true}}}), null);
  assert.equal(pagePerson({...person, issuer: {...person, capabilities: {stand_in: true}}}), 'ab');
});

test('assistant approval identifies the exact agent, workspace, apps, expiry and comparison code', () => {
  const request = {kind: 'agent', agent: 'agent-id', label: 'Assistant', workspace: 'workspace-id',
    apps: ['so.tilo.chat'], expiresAtMs: Date.UTC(2027, 1, 1), pairingCode: '123456'};
  const prompt = approvalPrompt(request, 'Home');
  for (const detail of ['agent-id', 'Assistant', 'workspace-id', 'Home', 'so.tilo.chat', '2027', '123456']) {
    assert.ok(prompt.includes(detail), detail);
  }
  assert.match(approvalPrompt({...request, apps: []}), /all apps/i);
});

test('a hello carries the certificate exactly as it was issued', () => {
  for (const {certified, hello: expected} of golden.cases) {
    const {cert} = parseExact(certified);
    assert.equal(typeof cert.issued_at_ns, 'bigint', 'nanoseconds are read exactly');
    assert.equal(hex(hello(golden.protocol, cert, bytes(golden.nonce))), expected);
  }
});

test('the server\'s challenge is read past its certificate', () => {
  assert.equal(hex(serverNonce(bytes(golden.ack))), golden.server_nonce);
});

test('the device signs the server\'s challenge under the client tag', async () => {
  let signed;
  const answer = await ack2(async digest => { signed = digest; return new Uint8Array(64).fill(4); }, bytes(golden.server_nonce));
  assert.equal(hex(signed), golden.digest);
  assert.equal(hex(answer), `40${'04'.repeat(64)}`);
});

test('blake3 of one block matches the reference', () => {
  // The BLAKE3 reference's hash of no input.
  assert.equal(hex(blake3Block(new Uint8Array(0))), 'af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262');
});

test('the page never says what the entry page says', () => {
  // `data-entry` is the entry page's word for how it took the browser in
  // (`idle`, `refused`, `failed`), and the app's web view reads it to know it
  // is still at the entry. The page the server renders is past the entry: it
  // sets it never, so a view that is held on an app's page cannot read its
  // own page as an entry it has not left.
  const script = readFileSync(new URL('./page.mjs', import.meta.url), 'utf8');
  assert.ok(!/dataset\.entry\b/.test(script), 'the page sets no data-entry');
  assert.ok(!/data-entry/.test(script), 'the page names no data-entry');
});
