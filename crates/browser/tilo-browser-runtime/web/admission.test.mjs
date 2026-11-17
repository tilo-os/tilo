import assert from 'node:assert/strict';
import test from 'node:test';
import {AdmissionError, NO_ACCESS, admit} from './admission.mjs';
import {CustodyError} from './device-custody.mjs';
import {runtimeFake} from './runtime-surface.mjs';

// Which way in, and the next one when a way fails, are the runtime's rules
// (tilo-session-client's `way_in`, tested there). These tests hold the page to
// taking the way it is answered, and to telling the runtime what it knows.

const target = {kind: 'workspace', host: 'https://ada.tilo.cloud', workspace: '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90'};
const commons = {name: 'Commons', host: {origin: target.host}, admission: `${target.host}/`};

/** A runtime answering `ways` in turn, the first to `wayIn` and each next to
 * whichever the page asks; `asAsked` answers whether an admission is the one
 * the link asked for. */
function rules(ways, {asAsked = true} = {}) {
  const asked = [];
  const next = (name, ...args) => {
    asked.push([name, ...args.map(arg => (typeof arg === 'string' && arg.startsWith('{') ? JSON.parse(arg) : arg))]);
    return ways.shift();
  };
  return {asked, runtime: runtimeFake({
    wayIn: (...args) => next('wayIn', ...args),
    wayInAfter: (...args) => next('wayInAfter', ...args),
    admittedAsAsked: (...args) => { asked.push(['admittedAsAsked', ...args]); return asAsked; },
  })};
}

function entries(overrides = {}) {
  const calls = [];
  const record = (name, value) => async (...args) => { calls.push([name, ...args]); return typeof value === 'function' ? value(...args) : value; };
  return {calls, deps: {
    workspaceAt: () => null,
    enterViaHost: record('enterViaHost', {admission: true, workspace: target.workspace}),
    openInvitation: record('openInvitation', (site, secret) => ({invitation: `opened ${secret}`})),
    inspectInvitation: record('inspectInvitation', {workspaceName: 'Reading group', role: 'editor', workspace: target.workspace, members: 4}),
    confirmInvitation: record('confirmInvitation', true),
    joinWorkspace: record('joinWorkspace', {joined: true}),
    readAccounts: record('readAccounts', []),
    chooseAccount: record('chooseAccount', accounts => accounts[0]),
    connectMember: record('connectMember', {member: true}),
    redeemTicket: record('redeemTicket', {admission: 'ticket'}),
    pairDevice: record('pairDevice', {admission: 'paired'}),
    reconnect: record('reconnect', null),
    ...overrides,
  }};
}

test('the runtime is told what the link carries and what the page knows of its host, and asked again once accounts are read', async () => {
  const {asked, runtime} = rules(['accounts', 'entry']);
  const {calls, deps} = entries({workspaceAt: origin => (origin === target.host ? commons : null),
    entered: ({origin, workspace: id}) => origin === target.host && id === 'elsewhere'});
  assert.deepEqual(await admit({scope: target, secrets: {}, tickets: 'https://workspaces.example/', runtime, deps}),
    {peer: {admission: true, workspace: target.workspace}});
  const here = {entry: true, entered: false, tickets: true, pairing: true};
  assert.deepEqual(asked.slice(0, 2), [
    ['wayIn', {scope: {kind: 'workspace', workspace: target.workspace}, malformed: false, invite: false, ticket: false}, here],
    ['wayIn', {scope: {kind: 'workspace', workspace: target.workspace}, malformed: false, invite: false, ticket: false}, {...here, accounts: 0}],
  ]);
  // The entry is asked at the workspace the link names, not the listed one; the
  // peer it answers with is held to that workspace (`admittedAsAsked` below).
  assert.deepEqual(calls, [['readAccounts', target.host], ['enterViaHost',
    {workspace: {...commons, workspace: target.workspace, admission: `${target.host}/directory/${target.workspace}/`}}]]);
  // A pairing secret is read by the page: the runtime hears whether it names someone.
  const pairing = rules(['pair']);
  await admit({scope: target, secrets: {pair: 'ab'.repeat(32)}, runtime: pairing.runtime, deps: entries().deps});
  assert.equal(pairing.asked[0][1].pair, 'named');
  const damaged = rules(['malformed-pairing']);
  await assert.rejects(admit({scope: target, secrets: {pair: 'ab'}, runtime: damaged.runtime, deps: entries().deps}), {code: 'malformed'});
  assert.equal(damaged.asked[0][1].pair, 'damaged');
});

test('a link into another workspace of the host asks at that workspace’s own entry, not the listed home’s', async () => {
  // A host lists its home workspace and its entry. A link names another of its
  // workspaces — one this browser never entered, such as the person's own —
  // and the home's entry would let it into the home, then hold it to the named
  // workspace and refuse it. The entry asked is the named workspace's.
  const home = {name: 'Lounge', host: {origin: target.host}, workspace: 'b943a4e2-c020-4494-b087-a71a28bae296',
    admission: `${target.host}/peer/v1/directory/b943a4e2-c020-4494-b087-a71a28bae296/`};
  const {calls, deps} = entries({workspaceAt: () => home,
    enterViaHost: async ({workspace}) => { calls.push(['enterViaHost', workspace]); return {workspace: target.workspace}; }});
  assert.deepEqual(await admit({scope: target, runtime: rules(['entry']).runtime, deps}),
    {peer: {workspace: target.workspace}});
  assert.deepEqual(calls, [['enterViaHost', {...home, workspace: target.workspace,
    admission: `${target.host}/peer/v1/directory/${target.workspace}/`}]]);
  // A link naming the listed workspace keeps the entry the site names for it.
  const atHome = {...target, workspace: home.workspace};
  await admit({scope: atHome, runtime: rules(['entry']).runtime, deps});
  assert.deepEqual(calls.at(-1), ['enterViaHost', home]);
});

test('the person’s own desktop needs no peer', async () => {
  const {calls, deps} = entries();
  assert.deepEqual(await admit({scope: {kind: 'own'}, runtime: rules(['own-desktop']).runtime, deps}), {peer: null});
  assert.deepEqual(calls, []);
});

test('the own desktop of a stranger under a home taking people by invitation is that home’s entry', async () => {
  // Going to /me directly: the home's entry answers, as at the bare entry,
  // and its refusal (the wait list) is what the page shows.
  const turnedAway = Object.assign(new Error('Commons needs an invitation'), {code: 'invitation-only'});
  const asked = [];
  const {calls, deps} = entries({
    ownDesktopOpen: async home => { asked.push(home); return false; },
    enterViaHost: async options => { calls.push(['enterViaHost', options]); throw turnedAway; },
  });
  await assert.rejects(admit({scope: {kind: 'own'}, home: commons, runtime: rules(['own-desktop']).runtime, deps}), turnedAway);
  assert.deepEqual(asked, [commons]);
  assert.deepEqual(calls, [['enterViaHost', {workspace: commons}]]);
});

test('the own desktop opens without a peer for whom the home’s hold is open, or whom its entry just let in', async () => {
  const own = () => rules(['own-desktop']).runtime;
  const open = entries({ownDesktopOpen: async () => true});
  assert.deepEqual(await admit({scope: {kind: 'own'}, home: commons, runtime: own(), deps: open.deps}), {peer: null});
  assert.deepEqual(open.calls, [], 'a member asks no entry');
  let disposed = false;
  let kept = false;
  const letIn = entries({ownDesktopOpen: async () => kept,
    enterViaHost: async () => { kept = true; return {workspace: target.workspace, dispose: async () => { disposed = true; }}; }});
  assert.deepEqual(await admit({scope: {kind: 'own'}, home: commons, runtime: own(), deps: letIn.deps}), {peer: null});
  assert.ok(disposed, 'the entry’s peer is let go: the own desktop has none');
  // An entry with nothing to admit yet (an embed before its visitor's first
  // gesture) keeps no membership: still nothing of their own.
  const idle = entries({ownDesktopOpen: async () => false, enterViaHost: async () => ({idle: true})});
  await assert.rejects(admit({scope: {kind: 'own'}, home: commons, runtime: own(), deps: idle.deps}), {name: 'AdmissionError', code: 'no-access'});
  // A home without an entry holds nothing.
  const bare = entries({ownDesktopOpen: async () => false});
  assert.deepEqual(await admit({scope: {kind: 'own'}, home: {name: 'Team', host: {origin: target.host}}, runtime: own(), deps: bare.deps}), {peer: null});
  assert.deepEqual(bare.calls, []);
});

test('an embed the entry has nothing to admit yet mounts nothing', async () => {
  // Before its visitor's first gesture the page joins nothing and tells the
  // site to show its fallback: no peer, no mount.
  const {deps} = entries({workspaceAt: () => commons, enterViaHost: async () => ({idle: true})});
  assert.deepEqual(await admit({scope: target, runtime: rules(['entry']).runtime, deps}), {peer: null, idle: true});
});

test('the home workspace, whose workspace is not known yet, is entered through its host’s entry', async () => {
  const home = {kind: 'workspace', host: target.host, workspace: null};
  const {calls, deps} = entries({workspaceAt: () => commons});
  const {asked, runtime} = rules(['entry']);
  assert.deepEqual(await admit({scope: home, runtime, deps}), {peer: {admission: true, workspace: target.workspace}});
  assert.deepEqual(asked[0][1].scope, {kind: 'workspace'});
  assert.deepEqual(calls, [['enterViaHost', {workspace: commons}]], 'the bare entry names no workspace');
});

test('a person let in through the entry is not taken to another workspace on the host than the one they were admitted to', async () => {
  let disposed = false;
  const {deps} = entries({workspaceAt: () => commons,
    enterViaHost: async () => ({workspace: 'another-workspace', dispose: async () => { disposed = true; }})});
  const {asked, runtime} = rules(['entry'], {asAsked: false});
  await assert.rejects(admit({scope: target, runtime, deps}), {code: 'no-access', message: NO_ACCESS});
  assert.deepEqual(asked.at(-1), ['admittedAsAsked', target.workspace, 'another-workspace']);
  assert.ok(disposed);
});

test('an invitation is opened, inspected and confirmed before it is redeemed', async () => {
  const {calls, deps} = entries();
  const {peer} = await admit({scope: target, secrets: {invite: 'AAEC', token: 'tok'}, runtime: rules(['invitation']).runtime, deps});
  assert.deepEqual(peer, {joined: true});
  assert.deepEqual(calls.map(([name]) => name), ['openInvitation', 'inspectInvitation', 'confirmInvitation', 'joinWorkspace']);
  assert.equal(calls[0][1], target.host, 'the shelf is the workspace host’s');
  assert.equal(calls[0][2], 'AAEC', 'the fragment is the secret');
  assert.equal(calls[3][1].invitation, 'opened AAEC');
  assert.equal(calls[3][1].admissionToken, 'tok');
  const declined = entries({confirmInvitation: async () => false});
  await assert.rejects(admit({scope: target, secrets: {invite: 'AAEC'}, runtime: rules(['invitation']).runtime, deps: declined.deps}), {code: 'declined'});
  assert.ok(!declined.calls.some(([name]) => name === 'joinWorkspace'));
  // One for another workspace than the link names is refused before it is shown.
  const other = entries({inspectInvitation: async () => ({workspace: 'other', role: 'viewer'})});
  const mismatch = rules(['invitation'], {asAsked: false});
  await assert.rejects(admit({scope: target, secrets: {invite: 'AAEC'}, runtime: mismatch.runtime, deps: other.deps}), {code: 'mismatch'});
  assert.deepEqual(mismatch.asked.at(-1), ['admittedAsAsked', target.workspace, 'other']);
  assert.ok(!other.calls.some(([name]) => name === 'confirmInvitation'));
});

test('a link a site hands over opens at the site, and asks the host the invitation names', async () => {
  const sealed = {kind: 'sealed', site: 'https://tilo.sh', handle: 'abcdefghijklmnop'};
  const key = 'A'.repeat(43);
  const asked = [];
  const {calls, deps} = entries({
    openInvitation: async (site, secret) => {
      asked.push([site, secret]);
      return {invitation: 'the invitation', host: 'https://lounge.tilo.sh'};
    },
  });
  const way = () => rules(['sealed-invitation']).runtime;
  assert.deepEqual(await admit({scope: sealed, secrets: {invite: key}, runtime: way(), deps}), {peer: {joined: true}});
  assert.deepEqual(asked, [['https://tilo.sh', `abcdefghijklmnop${key}`]], 'the site keeps it; the key never leaves the page');
  assert.deepEqual(calls.map(([name]) => name), ['inspectInvitation', 'confirmInvitation', 'joinWorkspace']);
  assert.equal(calls[0][1], 'https://lounge.tilo.sh', 'the invitation is read at the host it admits into');
  assert.equal(calls[2][1].origin, 'https://lounge.tilo.sh');
  // A shelf of a build that named no host: the site's home workspace's host.
  const unnamed = entries({openInvitation: async () => ({invitation: 'x'})});
  await admit({scope: sealed, secrets: {invite: key}, home: commons, runtime: way(), deps: unnamed.deps});
  assert.equal(unnamed.calls.find(([name]) => name === 'joinWorkspace')[1].origin, target.host);
  // A shelf that does not answer, or a key that does not open it, is never
  // acted on: nothing is inspected, and the words are the ones a spent
  // invitation gets.
  const gone = entries({openInvitation: async () => { throw Object.assign(new Error('410'), {code: 'directory-error-invitation-gone'}); }});
  await assert.rejects(admit({scope: sealed, secrets: {invite: key}, runtime: way(), deps: gone.deps}), {code: 'invitation-unusable'});
  assert.ok(!gone.calls.some(([name]) => name === 'inspectInvitation'));
});

test('a way the runtime refuses asks nothing, and says why in the page’s words', async () => {
  for (const [way, code] of [['malformed', 'malformed'], ['malformed-pairing', 'malformed'], ['no-tickets', 'unsupported'],
    ['no-pairing', 'unsupported'], ['no-access', 'no-access']]) {
    const {calls, deps} = entries();
    const error = await admit({scope: target, secrets: {invite: 'x'}, runtime: rules([way]).runtime, deps}).catch(caught => caught);
    assert.ok(error instanceof AdmissionError, way);
    assert.equal(error.code, code, way);
    assert.deepEqual(calls, [], way);
  }
});

test('a member connects with their account for that host, chosen where there are several', async () => {
  const account = {userPublicKey: 'ab', profile: 'p', device: 'd'};
  const {calls, deps} = entries({readAccounts: async () => [account]});
  assert.deepEqual(await admit({scope: target, runtime: rules(['accounts', 'member']).runtime, deps}), {peer: {member: true}});
  assert.deepEqual(calls, [['connectMember', {origin: target.host, workspace: target.workspace, account}]]);
  const second = {userPublicKey: 'cd'};
  const several = entries({readAccounts: async () => [account, second], chooseAccount: async () => second});
  await admit({scope: target, runtime: rules(['accounts', 'member']).runtime, deps: several.deps});
  assert.equal(several.calls.at(-1)[1].account, second);
});

test('no access says the same thing for a missing and a private workspace', async () => {
  const member = {readAccounts: async () => [{userPublicKey: 'ab'}]};
  const failures = [
    entries({...member, connectMember: async () => { throw new Error('workspace 6f1c… is unknown on this host'); }}),
    entries({...member, connectMember: async () => { throw new Error('not a member of this workspace'); }}),
    entries({...member, connectMember: async () => ({whenReady: async () => { throw new Error('This workspace’s host no longer has you as a member.'); }})}),
    entries({...member, connectMember: async () => { throw new CustodyError('custody-missing', 'This account has no existing browser device'); }}),
  ];
  for (const {deps} of failures) {
    const {asked, runtime} = rules(['accounts', 'member', 'no-access']);
    const error = await admit({scope: target, runtime, deps}).catch(caught => caught);
    assert.deepEqual(asked.at(-1)[1], 'member', 'the runtime hears the member was refused');
    assert.ok(error instanceof AdmissionError);
    assert.equal(error.code, 'no-access');
    assert.equal(error.message, NO_ACCESS);
  }
});

test('a member whose device keys other tabs hold gets in as a new device, or is told so, never refused', async () => {
  const busy = () => { throw new CustodyError('custody-busy', 'Every device key is held by another tab'); };
  const member = {readAccounts: async () => [{userPublicKey: 'ab'}], connectMember: async () => busy()};
  // Where the page holds the person's root, this tab is a new device of theirs.
  const reconnected = entries({...member, reconnect: async ({origin, workspace: wanted}) => ({origin, workspace: wanted, admission: 'reconnect'})});
  const way = rules(['accounts', 'member', 'reconnect']);
  assert.deepEqual(await admit({scope: target, runtime: way.runtime, deps: reconnected.deps}),
    {peer: {origin: target.host, workspace: target.workspace, admission: 'reconnect'}});
  assert.deepEqual([way.asked.at(-1)[0], way.asked.at(-1)[2].accounts], ['wayIn', 0], 'asked again as a browser without the account');
  // Where it cannot, the page says other tabs hold them: another tab of this
  // browser says nothing about the host.
  const stuck = rules(['accounts', 'member', 'reconnect', 'no-access']);
  await assert.rejects(admit({scope: target, runtime: stuck.runtime, deps: entries(member).deps}), {name: 'CustodyError', code: 'custody-busy'});
  assert.deepEqual(stuck.asked.at(-1).slice(0, 2), ['wayInAfter', 'reconnect']);
  // As where the workspace cannot admit a new device of the person by itself.
  const uninvited = rules(['accounts', 'reconnect']);
  await assert.rejects(admit({scope: target, runtime: uninvited.runtime, deps: entries({reconnect: async () => busy()}).deps}),
    {name: 'CustodyError', code: 'custody-busy'});
  assert.ok(!uninvited.asked.some(([, failed]) => failed === 'reconnect'), 'the runtime is not asked what follows');
});

test('reconnecting says the page is connecting, so a host that does not answer is not blamed on this device', async () => {
  const progress = [];
  const back = entries({reconnect: async ({origin, workspace: wanted}) => ({origin, workspace: wanted})});
  await admit({scope: target, runtime: rules(['reconnect']).runtime, deps: back.deps, onProgress: event => progress.push(event.phase)});
  assert.deepEqual(progress, ['connecting']);
});

test('a host this browser set up or joined as its person is reconnected to, and the entry is the way in only when that fails', async () => {
  const back = entries({reconnect: async ({origin, workspace: wanted}) => ({owner: true, origin, workspace: wanted})});
  assert.deepEqual(await admit({scope: target, runtime: rules(['accounts', 'reconnect']).runtime, deps: back.deps}),
    {peer: {owner: true, origin: target.host, workspace: target.workspace}});
  for (const reconnect of [async () => null, async () => { throw new Error('not a member of this workspace'); }]) {
    const {calls, deps} = entries({workspaceAt: () => commons, reconnect});
    const {asked, runtime} = rules(['accounts', 'reconnect', 'entry']);
    assert.deepEqual(await admit({scope: target, runtime, deps}), {peer: {admission: true, workspace: target.workspace}});
    assert.deepEqual(asked.find(([name]) => name === 'wayInAfter').slice(0, 2), ['wayInAfter', 'reconnect'], 'the host’s reason is not passed on');
    assert.ok(calls.some(([name]) => name === 'enterViaHost'));
  }
});

test('a guest ticket is redeemed where the web host says, and a pairing link pairs this browser', async () => {
  const tickets = 'https://workspaces.example/workspaces/';
  const ticket = entries();
  assert.deepEqual(await admit({scope: target, secrets: {ticket: 't1'}, tickets, runtime: rules(['ticket']).runtime, deps: ticket.deps}),
    {peer: {admission: 'ticket'}});
  assert.deepEqual(ticket.calls, [['redeemTicket', {origin: target.host, workspace: target.workspace, ticket: 't1', tickets}]]);
  const person = 'ab'.repeat(32);
  const invite = '0f'.repeat(16);
  const pair = entries();
  assert.deepEqual(await admit({scope: target, secrets: {pair: `${person}${invite}`}, runtime: rules(['pair']).runtime, deps: pair.deps}),
    {peer: {admission: 'paired'}});
  assert.deepEqual(pair.calls, [['pairDevice', {origin: target.host, workspace: target.workspace, userPublicKey: person, invite, listed: null}]]);
});
