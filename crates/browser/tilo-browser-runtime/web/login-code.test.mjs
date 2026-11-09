import assert from 'node:assert/strict';
import test from 'node:test';
import {CREATE_ROUTE, FOLLOW_ROUTE, loginCodes, standInMessage} from './login-code.mjs';

test('a code is made and followed with the secret only this tab got', async () => {
  const sent = [];
  const answers = [{code: 'K7Q-MX2P', follow: 'ee'.repeat(32), expires_at_ms: 600_000}, {resolved: false, expired: false, steps: []}];
  const codes = loginCodes('https://eu1.tilo.sh/', async (url, init) => {
    sent.push([url, JSON.parse(init.body)]);
    return {ok: true, status: 200, json: async () => answers.shift()};
  });
  const made = await codes.create('https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab');
  assert.equal(made.code, 'K7Q-MX2P');
  assert.equal(made.expiresAt, 600_000);
  assert.deepEqual(await made.follow(), {resolved: false, expired: false, steps: []});
  assert.deepEqual(sent, [[`https://eu1.tilo.sh${CREATE_ROUTE}`, {link: 'https://tilo.sh/#/@eu1.tilo.sh/w/x?pair=ab'}],
    [`https://eu1.tilo.sh${FOLLOW_ROUTE}`, {follow: 'ee'.repeat(32)}]]);
  const signIn = loginCodes('https://eu1.tilo.sh/', async (url, init) => { sent.push(JSON.parse(init.body)); return {ok: true, status: 200, json: async () => ({})}; });
  await signIn.create(null);
  assert.deepEqual(sent.at(-1), {}, 'a sign-in code names no link');
});

test('a host that refuses says why, as a code the screen reads', async () => {
  const codes = loginCodes('https://eu1.tilo.sh/', async () => ({ok: false, status: 403}));
  await assert.rejects(codes.create(), {code: 'not-a-device', status: 403});
  const gone = loginCodes('https://eu1.tilo.sh/', async () => { throw new TypeError('offline'); });
  await assert.rejects(gone.create(), {code: 'unreachable', status: 0});
});

test('the stand-in message is the one the host checks: tag, op, root, time little-endian', () => {
  const message = standInMessage('key', '01'.repeat(32), 258n);
  const head = new TextEncoder().encode('tilo-stand-in-v1\0key\0');
  assert.deepEqual(message.slice(0, head.length), head);
  assert.deepEqual(message.slice(head.length, head.length + 32), new Uint8Array(32).fill(1));
  assert.deepEqual([...message.slice(-8)], [2, 1, 0, 0, 0, 0, 0, 0]);
});
