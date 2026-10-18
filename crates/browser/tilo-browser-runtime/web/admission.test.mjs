import assert from 'node:assert/strict';
import test from 'node:test';
import {AdmissionError, NO_ACCESS, admit} from './admission.mjs';
import {CustodyError} from './device-custody.mjs';

const room = {kind: 'room', host: 'https://quinten.tilo.cloud', room: '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90'};

function doors(overrides = {}) {
  const calls = [];
  const record = (name, value) => async (...args) => { calls.push([name, ...args]); return typeof value === 'function' ? value(...args) : value; };
  return {calls, deps: {
    workspaceAt: () => null,
    enterViaHost: record('enterViaHost', {admission: true, room: room.room}),
    inspectInvitation: record('inspectInvitation', {workspaceName: 'Reading group', role: 'editor', room: room.room}),
    confirmInvitation: record('confirmInvitation', true),
    joinWorkspace: record('joinWorkspace', {joined: true}),
    readAccounts: record('readAccounts', []),
    chooseAccount: record('chooseAccount', accounts => accounts[0]),
    connectMember: record('connectMember', {member: true}),
    redeemTicket: record('redeemTicket', {admission: 'ticket'}),
    pairDevice: record('pairDevice', {admission: 'paired'}),
    ...overrides,
  }};
}

test('the person’s own room needs no peer', async () => {
  const {calls, deps} = doors();
  assert.deepEqual(await admit({scope: {kind: 'own'}, deps}), {peer: null});
  assert.deepEqual(calls, []);
});

const commons = {name: 'Commons', host: {origin: room.host}, admission: `${room.host}/`};

test('a host whose listed workspace has an admission door lets the person in when the browser has no account there', async () => {
  const {calls, deps} = doors({workspaceAt: origin => (origin === room.host ? commons : null)});
  assert.deepEqual(await admit({scope: room, deps}), {peer: {admission: true, room: room.room}});
  // The door is told which room the link names: a host set up again is not
  // offered to be joined again from a link to a room it no longer has.
  assert.deepEqual(calls, [['readAccounts', room.host], ['enterViaHost', {workspace: commons, room: room.room}]]);
});

test('a member of a host with an admission door connects as the member', async () => {
  const account = {userPublicKey: 'ab', profile: 'p', device: 'd'};
  const {calls, deps} = doors({workspaceAt: () => commons, readAccounts: async () => [account]});
  assert.deepEqual(await admit({scope: room, deps}), {peer: {member: true}});
  assert.ok(!calls.some(([name]) => name === 'enterViaHost'));
});

test('the home workspace, whose room is not known yet, is entered through its host’s door or not at all', async () => {
  const home = {kind: 'room', host: room.host, room: null};
  const {calls, deps} = doors({workspaceAt: () => commons});
  assert.deepEqual(await admit({scope: home, deps}), {peer: {admission: true, room: room.room}});
  assert.deepEqual(calls, [['enterViaHost', {workspace: commons}]], 'the bare entry names no room');
  await assert.rejects(admit({scope: home, deps: doors().deps}), {code: 'no-access'});
});

test('a person let in through the door is not taken to another room on the host than the one they were admitted to', async () => {
  let disposed = false;
  const {deps} = doors({workspaceAt: () => commons,
    enterViaHost: async () => ({room: 'another-room', dispose: async () => { disposed = true; }})});
  await assert.rejects(admit({scope: room, deps}), {code: 'no-access', message: NO_ACCESS});
  assert.ok(disposed);
});

test('an invitation is inspected and confirmed before it is redeemed', async () => {
  const {calls, deps} = doors();
  const {peer} = await admit({scope: room, secrets: {invite: 'AAEC', token: 'tok'}, deps});
  assert.deepEqual(peer, {joined: true});
  assert.deepEqual(calls.map(([name]) => name), ['inspectInvitation', 'confirmInvitation', 'joinWorkspace']);
  assert.equal(calls[2][1].invitation, 'AAEC');
  assert.equal(calls[2][1].admissionToken, 'tok');
  const declined = doors({confirmInvitation: async () => false});
  await assert.rejects(admit({scope: room, secrets: {invite: 'AAEC'}, deps: declined.deps}), {code: 'declined'});
  assert.ok(!declined.calls.some(([name]) => name === 'joinWorkspace'));
});

test('an invitation for another room than the link names is refused', async () => {
  const {deps} = doors({inspectInvitation: async () => ({room: 'other', role: 'viewer'})});
  await assert.rejects(admit({scope: room, secrets: {invite: 'AAEC'}, deps}), {code: 'mismatch'});
});

test('a damaged secret is never used', async () => {
  const {calls, deps} = doors();
  await assert.rejects(admit({scope: room, secrets: {invite: 'x', malformed: true}, deps}), {code: 'malformed'});
  assert.deepEqual(calls, []);
});

test('a member connects with their account for that host', async () => {
  const account = {userPublicKey: 'ab', profile: 'p', device: 'd'};
  const {calls, deps} = doors({readAccounts: async () => [account]});
  assert.deepEqual(await admit({scope: room, deps}), {peer: {member: true}});
  assert.deepEqual(calls, [['connectMember', {origin: room.host, room: room.room, account}]]);
});

test('no access says the same thing for a missing and a private room', async () => {
  const none = doors();
  const noAccount = await admit({scope: room, deps: none.deps}).catch(error => error);
  const refused = doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => { throw new Error('room 6f1c… is unknown on this host'); },
  });
  const unknown = await admit({scope: room, deps: refused.deps}).catch(error => error);
  const forbidden = await admit({scope: room, deps: doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => { throw new Error('not a member of this room'); },
  }).deps}).catch(error => error);
  const revoked = await admit({scope: room, deps: doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => ({whenReady: async () => { throw new Error('This workspace’s host no longer has you as a member.'); }}),
  }).deps}).catch(error => error);
  for (const error of [noAccount, unknown, forbidden, revoked]) {
    assert.ok(error instanceof AdmissionError);
    assert.equal(error.code, 'no-access');
    assert.equal(error.message, NO_ACCESS);
  }
});

test('a member whose device keys other tabs hold gets in as a new device, or is told so, never refused', async () => {
  const busy = () => { throw new CustodyError('custody-busy', 'Every device key is held by another tab'); };
  // Where the page holds the person's root, this tab is a new device of theirs.
  const reconnected = doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => busy(),
    reconnect: async ({origin, room: wanted}) => ({origin, room: wanted, admission: 'reconnect'}),
  });
  assert.deepEqual(await admit({scope: room, deps: reconnected.deps}),
    {peer: {origin: room.host, room: room.room, admission: 'reconnect'}});
  // Where it cannot, the page says other tabs hold them: another tab of this
  // browser says nothing about the host.
  const {deps} = doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => busy(),
    reconnect: async () => null,
  });
  await assert.rejects(admit({scope: room, deps}), {name: 'CustodyError', code: 'custody-busy'});
  // As where the room cannot admit a new device of the person by itself.
  const uninvited = doors({reconnect: async () => busy()});
  await assert.rejects(admit({scope: room, deps: uninvited.deps}), {name: 'CustodyError', code: 'custody-busy'});
  const missing = doors({
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => { throw new CustodyError('custody-missing', 'This account has no existing browser device'); },
  });
  await assert.rejects(admit({scope: room, deps: missing.deps}), {name: 'AdmissionError', code: 'no-access', message: NO_ACCESS});
});

test('a guest ticket is redeemed where the web host says, and refused where it says nothing', async () => {
  const {calls, deps} = doors();
  const tickets = 'https://rooms.example/rooms/';
  assert.deepEqual(await admit({scope: room, secrets: {ticket: 't1'}, tickets, deps}), {peer: {admission: 'ticket'}});
  assert.deepEqual(calls, [['redeemTicket', {origin: room.host, room: room.room, ticket: 't1', tickets}]]);
  const bare = doors();
  await assert.rejects(admit({scope: room, secrets: {ticket: 't1'}, deps: bare.deps}), {code: 'unsupported'});
  assert.deepEqual(bare.calls, []);
});

test('a pairing link makes this browser a new device of the person it names, before any account is read', async () => {
  const {calls, deps} = doors();
  const person = 'ab'.repeat(32);
  assert.deepEqual(await admit({scope: room, secrets: {pair: person}, deps}), {peer: {admission: 'paired'}});
  assert.deepEqual(calls, [['pairDevice', {origin: room.host, room: room.room, userPublicKey: person, invite: null, workspace: null}]]);
  const bare = doors({pairDevice: undefined});
  await assert.rejects(admit({scope: room, secrets: {pair: person}, deps: bare.deps}), {code: 'unsupported'});
});

test('a pairing link that names an invite passes it on, and a damaged one is refused before anything is asked', async () => {
  const {calls, deps} = doors();
  const person = 'ab'.repeat(32);
  const invite = '0f'.repeat(16);
  await admit({scope: room, secrets: {pair: `${person}${invite}`}, deps});
  assert.deepEqual(calls, [['pairDevice', {origin: room.host, room: room.room, userPublicKey: person, invite, workspace: null}]]);
  for (const pair of [person.slice(2), `${person}${invite.slice(2)}`, person.toUpperCase(), `${person}${invite}00`]) {
    const refused = doors();
    await assert.rejects(admit({scope: room, secrets: {pair}, deps: refused.deps}), {code: 'malformed'});
    assert.deepEqual(refused.calls, []);
  }
});

test('a host this browser set up or joined as its person is reconnected to, and anything else stays no access', async () => {
  const {calls, deps} = doors({reconnect: async ({origin, room: wanted}) => (origin === room.host ? {owner: true, room: wanted} : null)});
  assert.deepEqual(await admit({scope: room, deps}), {peer: {owner: true, room: room.room}});
  assert.deepEqual(calls.map(([name]) => name), ['readAccounts']);
  await assert.rejects(admit({scope: {...room, host: 'https://elsewhere.example'}, deps}), {code: 'no-access', message: NO_ACCESS});
  const refused = doors({reconnect: async () => { throw new Error('not a member of this room'); }});
  await assert.rejects(admit({scope: room, deps: refused.deps}), {code: 'no-access', message: NO_ACCESS});
});
