import test from 'node:test';
import assert from 'node:assert/strict';
import {openResourceOperation, closeResourceOperation} from './resource-session.mjs';
const scope = {origin: 'https://host.example', room: 'fixture-room', hostDevice: 'ab'.repeat(32)};
const address = {...scope, version: 1, provider: 'record:so.tilo.chat:fixture:1',
  reference: {kind: 'chat.message', id: 'chat.message:m1'}, action: 'reveal'};
const link = 'https://web.example/resource.html#resource=' + encodeURIComponent(JSON.stringify(address));
const operation = () => ({controller: new AbortController()});
function fixture(open) {
  const calls = [];
  const mount = {cleanup: async () => { calls.push('cleanup'); }};
  const peer = {resourceScope: scope, whenReady: async () => {},
    openResource: (...args) => open ? open(...args) : Promise.resolve(mount), dispose: async () => { calls.push('dispose'); }};
  return {calls, mount, peer};
}
test('the browser page owns navigation and Leave releases the mounted app and peer', async () => {
  const op = operation(); const f = fixture();
  assert.equal(await openResourceOperation({operation: op, link, target: 'surface',
    connect: async (seen, signal) => { assert.deepEqual(seen, address); assert.equal(signal, op.controller.signal); return f.peer; }}), f.mount);
  assert.deepEqual(f.calls, []);
  await closeResourceOperation(op);
  assert.equal(op.controller.signal.aborted, true);
  assert.deepEqual(f.calls, ['cleanup', 'dispose']);
});
test('Leave during connection disposes a late peer without opening its resource', async () => {
  const op = operation(); const f = fixture(async () => { assert.fail('cancelled connection must not navigate'); });
  let keyClosed = false;
  op.custody = {close: async () => { keyClosed = true; }};
  let release; const waiting = new Promise(resolve => { release = resolve; });
  const opening = openResourceOperation({operation: op, link, target: 'surface', connect: () => waiting});
  await closeResourceOperation(op);
  assert.equal(keyClosed, true, 'Leave releases custody while connection is still pending');
  release(f.peer);
  await assert.rejects(opening, {name: 'AbortError'});
  assert.deepEqual(f.calls, ['dispose']);
});
test('Leave during hydration cleans a late mount and cannot affect a replacement session', async () => {
  const op = operation(); const replacement = operation(); const newer = fixture();
  let release; let started; const ready = new Promise(resolve => { started = resolve; });
  const waiting = new Promise(resolve => { release = resolve; });
  const f = fixture(async () => { started(); return waiting; });
  const opening = openResourceOperation({operation: op, link, target: 'old', connect: async () => f.peer});
  await ready; await closeResourceOperation(op);
  await openResourceOperation({operation: replacement, link, target: 'new', connect: async () => newer.peer});
  release(f.mount); await assert.rejects(opening, {name: 'AbortError'});
  assert.deepEqual(f.calls, ['dispose', 'cleanup', 'dispose']);
  assert.deepEqual(newer.calls, []);
  assert.equal(replacement.controller.signal.aborted, false);
  await closeResourceOperation(replacement);
});
test('app cleanup failure still disposes the peer', async () => {
  const op = operation(); const f = fixture();
  op.peer = f.peer; op.mount = {cleanup: async () => { throw Error('fixture cleanup failed'); }};
  await assert.rejects(closeResourceOperation(op), /fixture cleanup failed/);
  assert.deepEqual(f.calls, ['dispose']);
});
test('revocation or unavailable admission refuses before any app can remain open', async () => {
  const op = operation(); const f = fixture(async () => { throw Error('resource permission revoked'); });
  await assert.rejects(openResourceOperation({operation: op, link, target: 'surface', connect: async () => f.peer}), /revoked/);
  assert.deepEqual(f.calls, ['dispose']);
  assert.equal(op.controller.signal.aborted, true);
  const refused = operation();
  await assert.rejects(openResourceOperation({operation: refused, link, target: 'surface',
    connect: async () => { throw Error('existing certificate required'); }}), /certificate required/);
  assert.equal(refused.controller.signal.aborted, true);
});

test('key custody is released even when peer disposal fails', async () => {
  const op = operation(); let closed = false;
  op.peer = {dispose: async () => { throw Error('fixture peer disposal failed'); }};
  op.custody = {close: async () => { closed = true; }};
  await assert.rejects(closeResourceOperation(op), /peer disposal failed/);
  assert.equal(closed, true);
});
