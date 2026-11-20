// The page's handshake, byte for byte as the server's types encode it
// (`handshake.json`, which `tests.rs` checks against those types).
import {test} from 'bun:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {ack2, blake3Block, hello, parseExact, serverNonce} from './page.mjs';

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

test('blake3 of one block matches the reference', () => {
  // The BLAKE3 reference's hash of no input.
  assert.equal(hex(blake3Block(new Uint8Array(0))), 'af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262');
});
