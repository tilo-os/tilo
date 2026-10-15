// Tests of the end-to-end authentication check's own reading
// (dev/auth-e2e.mjs); the run itself needs the built stack and Chrome, which
// the `Browser / authentication` workflow has.
import assert from 'node:assert/strict';
import test from 'node:test';
import {joinsTo, membershipFor, parseArgs, parseBanner, SETTLED} from './auth-e2e.mjs';

test('the arguments name every input, absolute', () => {
  const options = parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o',
    '--expect-revision', 'abc', '--timeout', '60']);
  assert.ok(options.bundle.endsWith('/b') && options.relayBin.endsWith('/r') && options.out.endsWith('/o'));
  assert.equal(options.expectRevision, 'abc');
  assert.equal(options.timeout, 60);
  assert.equal(options.allowDev, false);
  assert.throws(() => parseArgs(['--bundle', 'b']), /usage/);
  assert.throws(() => parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o', '--x']), /unknown/);
  assert.throws(() => parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o', '--timeout', '0']),
    /positive/);
  assert.throws(() => parseArgs(['--out']), /needs a value/);
});

test("run.py's banner gives the room", () => {
  const banner = "\nTilo is up in 3.2s, room 'auth' (0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001), bundle abc\n";
  assert.deepEqual(parseBanner(`run.py: $ something\n${banner}`), {room: '0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001'});
  assert.equal(parseBanner('run.py: building'), null);
});

test('the membership is read by normalized origin, and nothing else counts', () => {
  const kept = JSON.stringify({'http://localhost:9764': {origin: 'http://localhost:9764', room: 'r', userPublicKey: 'k'}});
  assert.deepEqual(membershipFor(kept, 'http://localhost:9764/'), {origin: 'http://localhost:9764', room: 'r', userPublicKey: 'k'});
  assert.equal(membershipFor(kept, 'http://localhost:9765'), null);
  assert.equal(membershipFor(null, 'http://localhost:9764'), null);
  assert.equal(membershipFor('not json', 'http://localhost:9764'), null);
});

test('only POSTs to the door are joins', () => {
  const requests = [
    {url: 'http://localhost:8790/join', method: 'POST'},
    {url: 'http://localhost:8790/join', method: 'OPTIONS'},
    {url: 'http://localhost:8790/presence', method: 'GET'},
    {url: 'http://localhost:8791/join', method: 'POST'},
  ];
  assert.deepEqual(joinsTo(requests, 'http://localhost:8790'), [requests[0]]);
});

test('the settled entry states are the ones the entry page sets', () => {
  assert.deepEqual(SETTLED, ['open', 'refused', 'failed', 'unsupported']);
});
