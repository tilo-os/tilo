// The page's handshake, byte for byte as the server's types encode it
// (`handshake.json`, which `tests.rs` checks against those types).
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {ack2, blake3, browserNotices, hello, parseExact, serverNonce, signedAct} from './page.mjs';

const golden = JSON.parse(readFileSync(new URL('./handshake.json', import.meta.url), 'utf8'));
const bytes = text => Uint8Array.from(text.match(/../g) ?? [], pair => parseInt(pair, 16));
const hex = data => Array.from(data, b => b.toString(16).padStart(2, '0')).join('');

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

test('blake3 matches the reference across blocks and chunks', () => {
  for (const {len, hash} of golden.signing.hashes) {
    const input = Uint8Array.from({length: len}, (_, i) => i % 251);
    assert.equal(hex(blake3(input)), hash, `${len} bytes`);
  }
});

test('an act goes signed as the server checks it', async () => {
  const {act, message, call} = golden.signing;
  const {cert} = parseExact(golden.cases[0].certified);
  let signedOver;
  const sign = async digest => { signedOver = digest; return new Uint8Array(64).fill(4); };
  const made = await signedAct({cert, sign}, call.signed.workspace, 'notes', act,
    {invocation: call.signed.invocation, issuedAtNs: call.signed.issued_at_ns});
  assert.equal(hex(signedOver), message);
  assert.deepEqual(JSON.parse(JSON.stringify(made)), call);
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

// ---- The person's notifications, while the page is not in front ----

/** The browser's own notifications, as the page raises and closes them. */
class Raised {
  static all = [];
  static permission = 'granted';
  constructor(title, options) {
    this.title = title;
    this.options = options;
    this.closed = false;
    Raised.all.push(this);
  }
  close() {
    this.closed = true;
    this.onclose?.();
  }
}

/** A page's notifications, with the page in front until `hide()`. */
function notices() {
  Raised.all = [];
  Raised.permission = 'granted';
  const page = {hidden: false, focused: 0, answered: []};
  const shown = browserNotices({
    hidden: () => page.hidden,
    Notification: Raised,
    answer: id => page.answered.push(id),
    focus: () => {
      page.focused += 1;
      page.hidden = false;
    },
  });
  return {page, shown, raised: () => Raised.all.filter(notification => !notification.closed)};
}

const notice = (id, title, extra = {}) => ({
  id, app_name: 'Mail', title, body: '', urgency: 0, ...extra,
});

test('a notice is a browser notification only while the page is not in front', () => {
  const {page, shown, raised} = notices();
  const told = notice('7f', 'A message from Ada', {body: 'Lunch?', urgency: 2});
  // A page in front shows its own view: nothing is raised over it.
  shown.follow([told]);
  assert.deepEqual(raised(), []);

  page.hidden = true;
  shown.visibilityChanged();
  const [notification] = raised();
  assert.equal(notification.title, 'A message from Ada');
  assert.equal(notification.options.body, 'Lunch?');
  assert.equal(notification.options.tag, '7f', 'one per notice, however many pages raise it');
  assert.equal(notification.options.requireInteraction, true, 'an error stays until it is read');

  // The same set again raises nothing more.
  shown.follow([told]);
  assert.equal(raised().length, 1);

  // Back in front, the page's own view is what shows it.
  page.hidden = false;
  shown.visibilityChanged();
  assert.deepEqual(raised(), []);
  assert.deepEqual(page.answered, [], 'nobody dismissed it');
});

test('what the person dismisses in the browser answers it in the record', () => {
  const {page, shown, raised} = notices();
  page.hidden = true;
  shown.follow([notice('7f', 'A message from Ada')]);

  // The browser's own × is the person dismissing it: the record takes it off every device.
  const [notification] = raised();
  notification.closed = true;
  notification.onclose();
  assert.deepEqual(page.answered, ['7f']);
  assert.deepEqual(raised(), []);

  // The record shows it for another moment, while the answer is on its way: it stays dismissed.
  shown.follow([notice('7f', 'A message from Ada')]);
  assert.deepEqual(raised(), []);
  assert.deepEqual(page.answered, ['7f']);
});

test('a notice clicked brings the page in front and is answered', () => {
  const {page, shown, raised} = notices();
  page.hidden = true;
  shown.follow([notice('7f', 'A message from Ada'), notice('80', 'A reply', {urgency: 1})]);
  assert.equal(raised().length, 2);

  raised().find(notification => notification.options.tag === '80').onclick();
  assert.equal(page.focused, 1, 'the page comes forward');
  assert.deepEqual(page.answered, ['80'], 'and the notice leaves every device');
  assert.deepEqual(raised().map(notification => notification.options.tag), ['7f']);

  // The page is in front now: the rest goes too, without being answered.
  shown.visibilityChanged();
  assert.deepEqual(raised(), []);
  assert.deepEqual(page.answered, ['80']);
});

test('a notice that leaves the record is taken away without an answer', () => {
  const {page, shown, raised} = notices();
  page.hidden = true;
  shown.follow([notice('7f', 'A message from Ada'), notice('80', 'A reply')]);
  assert.equal(raised().length, 2);

  // Answered on another device of the person's, or run out: it is no longer shown.
  shown.follow([notice('80', 'A reply')]);
  assert.deepEqual(raised().map(notification => notification.options.tag), ['80']);
  assert.deepEqual(page.answered, [], 'the person here did not answer it');

  // The record reaches nothing here any more (the session ended).
  shown.follow([]);
  assert.deepEqual(raised(), []);
  assert.deepEqual(page.answered, []);
});

test('nothing is raised until the person lets the browser show them', () => {
  const {page, shown, raised} = notices();
  Raised.permission = 'default';
  page.hidden = true;
  shown.follow([notice('7f', 'A message from Ada')]);
  assert.deepEqual(raised(), [], 'a browser that was not let shows none');

  // They let it from a press of theirs: what the record holds is raised from then on.
  Raised.permission = 'granted';
  shown.visibilityChanged();
  assert.deepEqual(raised().map(notification => notification.options.tag), ['7f']);
});
