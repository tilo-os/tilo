import test from 'node:test';
import assert from 'node:assert/strict';
import {createWorkspaceActions, mayRemove, PEOPLE} from './workspace-actions.mjs';

const OWNER = 'a1'.repeat(32);
const KEEPER = 'a2'.repeat(32);
const ADMIN = 'a3'.repeat(32);
const EDITOR = 'a4'.repeat(32);
const WORKSPACE = '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90';

/** A workspace's member list as the host answers it. */
const members = () => [
  {member: OWNER, name: 'Ada', role: 'owner'},
  {member: KEEPER, name: 'Kai', role: 'editor', keeper: true},
  {member: ADMIN, name: 'Amin', role: 'editor', administrator: true},
  {member: EDITOR, name: 'Eve', role: 'editor'},
];

test('the owner holds everyone but themselves and nobody removes the owner', () => {
  const list = members();
  assert.equal(mayRemove(list, OWNER, list[3]), true, 'the owner removes a member');
  assert.equal(mayRemove(list, OWNER, list[2]), true, 'the owner removes an administrator');
  assert.equal(mayRemove(list, OWNER, list[0]), false, 'nobody removes the owner');
  assert.equal(mayRemove(list, KEEPER, list[0]), false);
  assert.equal(mayRemove(list, EDITOR, list[3]), false, 'a member cannot remove themselves');
});

test('an administrator reaches every member who is not an administrator', () => {
  const list = members();
  assert.equal(mayRemove(list, ADMIN, list[3]), true);
  assert.equal(mayRemove(list, ADMIN, list[1]), true);
  assert.equal(mayRemove(list, ADMIN, list[2]), false, 'one administrator never removes another');
  assert.equal(mayRemove(list, ADMIN, list[0]), false);
});

test('the person a workspace is kept for removes administrators too', () => {
  const list = members();
  assert.equal(mayRemove(list, KEEPER, list[2]), true);
  assert.equal(mayRemove(list, KEEPER, list[0]), false);
});

test('a plain member is allowed to remove nobody', () => {
  const list = members();
  for (const target of list) assert.equal(mayRemove(list, EDITOR, target), false, target.name);
  assert.equal(mayRemove(list, EDITOR, undefined), false);
  assert.equal(mayRemove([], EDITOR, {member: EDITOR}), false, 'a list without this person allows nothing');
});

/** The page in a workspace, as the host answers it. */
function room() {
  const calls = [];
  return {calls, peer: {
    origin: 'https://node.example:8443', workspace: WORKSPACE, userPublicKey: OWNER,
    host: {hostCert: {user_public_key: Array(32).fill(0xa1)}},
    createInvitation: async () => 'SIGNED-invitation',
    membership: {
      list: async () => { calls.push(['list']); return {workspace: WORKSPACE, name: 'Acme', members: members()}; },
      mayInvite: async () => false,
      waitList: async () => [],
      remove: async person => { calls.push(['remove', person]); return {workspace: WORKSPACE, members: []}; },
    },
  }};
}

test('the workspace offers People, and choosing it shows the host’s list', async () => {
  const {peer, calls} = room();
  const shown = [];
  let opened = null;
  const actions = createWorkspaceActions({peer, inviteLink: async () => 'https://tilo.so/i/x#y',
    ui: {people: async options => { opened = options; }, fail: async text => assert.fail(text)}});
  await actions.attach({setHostActions: list => shown.push(list)});
  assert.ok(shown.at(-1).some(action => action.id === PEOPLE.id), 'People is one of the workspace actions');
  assert.ok(actions.handles(PEOPLE.id));
  await actions.perform(PEOPLE.id);
  assert.ok(opened, 'the press opens the People card');
  assert.equal(opened.self, OWNER);
  const list = await opened.load();
  assert.equal(list.name, 'Acme');
  assert.deepEqual(list.members.map(member => member.name), ['Ada', 'Kai', 'Amin', 'Eve']);
  await opened.remove(EDITOR);
  assert.deepEqual(calls.at(-1), ['remove', EDITOR], 'removal is the host’s own membership request');
});

test('a page outside a workspace offers no People list', async () => {
  const shown = [];
  const actions = createWorkspaceActions({peer: null, ui: {}});
  await actions.attach({setHostActions: list => shown.push(list)});
  assert.ok(!shown.at(-1).some(action => action.id === PEOPLE.id));
});
