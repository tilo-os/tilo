import test from 'node:test';
import assert from 'node:assert/strict';
import {CREATE_OWN, OwnWorkspaceError, ensureOwnWorkspace, invitationText} from './own-workspace.mjs';
import {enteredWorkspaces} from './workspaces.mjs';

const HOST_DEVICE = 'ab'.repeat(32);
const ROOM = '0f0e0d0c-0b0a-4908-8706-050403020100';

function memoryStore() {
  const kept = new Map();
  return {getItem: key => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value)), removeItem: key => kept.delete(key)};
}

/** A peer in the site's home workspace whose host answers `answer(argument)`
 * to `room.create-own`, recording what it was asked. */
function lounge(answer, asked = [], admitted = []) {
  return {
    origin: 'https://tilo.example',
    displayName: 'Brave Otter',
    admitTo: async invitation => { admitted.push(invitation); return {workspace: ROOM}; },
    host: {hostCert: {device_public_key: HOST_DEVICE}},
    actions: {
      invoke(target, argument, options) {
        asked.push({target, argument, options});
        return {done: Promise.resolve().then(() => answer(argument))};
      },
    },
  };
}

const made = (name = 'My workspace') => ({
  state: 'succeeded',
  update: {output: {value: {workspace: ROOM, workspace_id: 7, name, generation: 3,
    invitation: {invite: '00'.repeat(32), invitation: 'fbff00'}}}},
});

test('the first time, the host makes the workspace, the page\'s own device is admitted to it, and the page remembers it', async () => {
  const asked = [];
  const admitted = [];
  const store = memoryStore();
  const own = await ensureOwnWorkspace({peer: lounge(() => made("Ada's workspace"), asked, admitted)}, {name: "Ada's workspace", store});
  assert.deepEqual(asked.map(({target, argument}) => ({target, argument})), [{target: CREATE_OWN, argument: {name: "Ada's workspace"}}]);
  assert.deepEqual(admitted, ['-_8A']);
  assert.deepEqual(own, {id: ROOM, name: "Ada's workspace", host: 'tilo.example', origin: 'https://tilo.example'});
  assert.deepEqual(enteredWorkspaces(store), [{origin: 'https://tilo.example', room: ROOM, hostDevice: HOST_DEVICE, name: "Ada's workspace", unavailable: false}]);
});

test('a workspace this browser entered before is answered without redeeming again', async () => {
  const store = memoryStore();
  const admitted = [];
  const page = {peer: lounge(() => made(), [], admitted)};
  await ensureOwnWorkspace(page, {store});
  const again = await ensureOwnWorkspace(page.peer, {store});
  assert.equal(admitted.length, 1);
  assert.equal(again.id, ROOM);
  assert.equal(enteredWorkspaces(store).length, 1);
});

test('a refusal carries the host\'s typed refusal', async () => {
  const refusal = {reason: {too_many_workspaces: {limit: 1, existing: [ROOM]}}, message: 'You already have a workspace of your own on this host; use that one.', next: [{kind: 'use_existing', url: null}]};
  const page = {peer: lounge(() => { throw Object.assign(new Error(JSON.stringify(refusal)), {code: 'refused'}); })};
  await assert.rejects(ensureOwnWorkspace(page, {store: memoryStore()}), error => {
    assert.ok(error instanceof OwnWorkspaceError);
    assert.equal(error.code, 'refused');
    assert.equal(error.message, refusal.message);
    assert.deepEqual(error.refusal, refusal);
    return true;
  });
  // A host that keeps none offers no such action.
  const none = {peer: lounge(() => { throw Object.assign(new Error('unknown action'), {code: 'unknown_action'}); })};
  await assert.rejects(ensureOwnWorkspace(none, {store: memoryStore()}), error => error.code === 'unknown_action' && error.refusal === null);
});

test('a page in no workspace has nothing to ask from', async () => {
  await assert.rejects(ensureOwnWorkspace({peer: null}), TypeError);
  await assert.rejects(ensureOwnWorkspace({peer: {origin: 'https://tilo.example'}}), TypeError);
});

test('an invitation travels as unpadded URL-safe base64', () => {
  assert.equal(invitationText('fbff00'), '-_8A');
  assert.equal(invitationText('00'), 'AA');
  assert.throws(() => invitationText('zz'), TypeError);
});
