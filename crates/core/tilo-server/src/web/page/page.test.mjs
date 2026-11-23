// The page's handshake, byte for byte as the server's types encode it
// (`handshake.json`, which `tests.rs` checks against those types).
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {ack2, blake3, hello, parseExact, serverNonce, signedAct} from './page.mjs';

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
