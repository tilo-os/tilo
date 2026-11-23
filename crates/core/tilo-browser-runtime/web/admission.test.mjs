import assert from 'node:assert/strict';
import test from 'node:test';
import {AdmissionError, NO_ACCESS, admit} from './admission.mjs';
import {DeviceKeyError} from './device-key.mjs';
import {runtimeFake} from './runtime-surface.mjs';

// Which way in, and the next one when a way fails, are the runtime's rules
// (tilo-session-client's `way_in`, tested there). These tests hold the page to
// taking the way it is answered, and to telling the runtime what it knows.

const target = {kind: 'workspace', host: 'https://ada.tilo.cloud', workspace: '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90'};
const commons = {name: 'Commons', host: {origin: target.host}, admission: `${target.host}/`};
/** The site's listed workspace on that server, its entry letting anyone in. */
const lounge = {...commons, name: 'Lounge', admission: `${commons.admission}peer/v1/directory/b943a4e2-c020-4494-b087-a71a28bae296/`};

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
    openInvitation: record('openInvitation', (origin, workspace, secret) => `opened ${secret}`),
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
  const here = {entry: true, listed: false, person: false, entered: false, tickets: true, pairing: true};
  assert.deepEqual(asked.slice(0, 2), [
    ['wayIn', {scope: {kind: 'workspace', workspace: target.workspace}, malformed: false, invite: false, ticket: false}, here],
    ['wayIn', {scope: {kind: 'workspace', workspace: target.workspace}, malformed: false, invite: false, ticket: false}, {...here, accounts: 0}],
  ]);
  // The entry is asked at the workspace the link names, not the listed one; the
  // peer it answers with is held to that workspace (`admittedAsAsked` below).
  assert.deepEqual(calls, [['readAccounts', target.host], ['enterViaHost',
    {workspace: {...commons, workspace: target.workspace, name: null, admission: `${target.host}/directory/${target.workspace}/`}}]]);
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
  // Not under the listed workspace's name: it is another workspace.
  assert.deepEqual(calls, [['enterViaHost', {...home, workspace: target.workspace, name: null,
    admission: `${target.host}/peer/v1/directory/${target.workspace}/`}]]);
  // A link naming the listed workspace keeps the entry the site names for it.
  const atHome = {...target, workspace: home.workspace};
  await admit({scope: atHome, runtime: rules(['entry']).runtime, deps});
  assert.deepEqual(calls.at(-1), ['enterViaHost', home]);
});

test('nobody signed in at a link to another workspace of the host is asked to sign in, before anything joins', async () => {
  let signedIn = false;
  const {calls, deps} = entries({workspaceAt: () => lounge, holdsPerson: async () => signedIn});
  const {asked, runtime} = rules(['accounts', 'sign-in']);
  await assert.rejects(admit({scope: target, runtime, deps}), {name: 'AdmissionError', code: 'sign-in',
    // Signing in pairs through the site's listed entry, as any sign-in does.
    entry: lounge.admission});
  // The rules hear that the link names another workspace than the listed one, and that nobody is here.
  assert.equal(asked[0][2].listed, false);
  assert.equal(asked[0][2].person, false);
  assert.deepEqual(calls, [['readAccounts', target.host]], 'no entry was asked and nobody joined');
  // The listed workspace's own link is that entry's to answer; someone signed in is said so.
  signedIn = true;
  const listed = rules(['accounts', 'entry']);
  await admit({scope: {...target, workspace: 'b943a4e2-c020-4494-b087-a71a28bae296'}, runtime: listed.runtime, deps});
  assert.equal(listed.asked[0][2].listed, true);
  assert.equal(listed.asked[0][2].person, true);
});

test('nobody signed in at a link to a public workspace is let in through its own entry, as its server’s directory lists it', async () => {
  const ownEntry = `${commons.admission}peer/v1/directory/${target.workspace}/`;
  const listings = [];
  const read = [];
  const {calls, deps} = entries({workspaceAt: () => lounge, holdsPerson: async () => false,
    listings: async origin => { read.push(origin); return listings; }});
  // Listed, and open to anyone: its own entry, named as it lists itself.
  listings.splice(0, Infinity, {workspace: target.workspace, name: 'Reading group', entry: ownEntry, invitationOnly: false, waitList: null});
  const open = rules(['accounts', 'entry']);
  await admit({scope: target, runtime: open.runtime, deps});
  assert.deepEqual(read, [commons.admission.slice(0, -1)], 'its server’s directory was read before anything joined');
  assert.equal(open.asked[0][2].listed, true, 'the rules hear that its entry lets anyone in');
  assert.deepEqual(calls.at(-1).slice(1), [{workspace: {...lounge, workspace: target.workspace, admission: ownEntry, name: 'Reading group'}}], 'its own entry');
  // Listed, but only for people invited: closed to a stranger, who signs in, told the listing.
  const closed = {workspace: target.workspace, name: 'Reading group', entry: ownEntry, invitationOnly: true,
    waitList: {contact: null, mail: true}};
  listings.splice(0, Infinity, closed);
  const invited = rules(['accounts', 'sign-in']);
  await assert.rejects(admit({scope: target, runtime: invited.runtime, deps}), {code: 'sign-in', listing: closed});
  assert.equal(invited.asked[0][2].listed, false);
  // Not listed, or the directory not read: closed alike.
  listings.splice(0, Infinity);
  const unlisted = rules(['accounts', 'sign-in']);
  await assert.rejects(admit({scope: target, runtime: unlisted.runtime, deps}), {code: 'sign-in', listing: null});
  const unread = entries({workspaceAt: () => lounge, holdsPerson: async () => false, listings: async () => { throw new Error('offline'); }});
  await assert.rejects(admit({scope: target, runtime: rules(['accounts', 'sign-in']).runtime, deps: unread.deps}), {code: 'sign-in'});
  // Someone signed in comes back as themself: the directory is not read.
  read.length = 0;
  const signedIn = entries({workspaceAt: () => lounge, holdsPerson: async () => true, listings: async origin => { read.push(origin); return []; }});
  await admit({scope: target, runtime: rules(['accounts', 'entry']).runtime, deps: signedIn.deps});
  assert.deepEqual(read, []);
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

test('reading an invitation says the page is connecting, before the workspace\'s server answers', async () => {
  const seen = [];
  const {deps} = entries({inspectInvitation: async () => { seen.push('inspect'); return null; }});
  await admit({scope: target, secrets: {invite: 'AAEC'}, runtime: rules(['invitation']).runtime, deps,
    onProgress: event => seen.push(event.phase)}).catch(() => {});
  assert.deepEqual(seen.slice(0, 2), ['connecting', 'inspect']);
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
  assert.deepEqual(calls[0].slice(1), [target.host, target.workspace, 'AAEC'],
    'asked of the workspace the link names, at its host, by the fragment’s secret');
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

test('an invitation whose server only it names is joined at that server, its offer read from it, nothing asked over HTTP', async () => {
  const invited = 'https://f97887d9895250ef7d60f7929cc947cc.iroh.invalid';
  const offer = {workspaceName: '', role: 'editor', workspace: target.workspace, members: null};
  const {calls, deps} = entries({invitedServer: async (invitation, origin) => {
    calls.push(['invitedServer', invitation, origin]);
    return {origin: invited, offer};
  }});
  const {peer} = await admit({scope: target, secrets: {invite: 'AAEC'}, runtime: rules(['invitation']).runtime, deps});
  assert.deepEqual(peer, {joined: true});
  assert.deepEqual(calls.map(([name]) => name), ['openInvitation', 'invitedServer', 'confirmInvitation', 'joinWorkspace']);
  assert.equal(calls[1][1], 'opened AAEC');
  assert.deepEqual(calls[2][1], offer, 'the card shows what the invitation itself offers');
  assert.equal(calls[3][1].origin, invited, 'the workspace is joined at the server the invitation names');
  // A server the link or its site names already is asked as it always was.
  const named = entries({invitedServer: async () => null});
  await admit({scope: target, secrets: {invite: 'AAEC'}, runtime: rules(['invitation']).runtime, deps: named.deps});
  assert.deepEqual(named.calls.map(([name]) => name), ['openInvitation', 'inspectInvitation', 'confirmInvitation', 'joinWorkspace']);
  assert.equal(named.calls[3][1].origin, target.host);
});

test('an invitation sent off the wait list opens the person’s own workspace in its place, unless its link names a place inside the workspace', async () => {
  // The short link a wait-list mail carries names the Lounge and nothing
  // inside it; the invitation admits at the Lounge's host.
  const short = {...target, workspace: 'lounge-id'};
  const key = 'A'.repeat(43);
  const server = target.host;
  const offer = waitList => ({workspaceName: 'Lounge', role: 'editor', workspace: 'lounge-id', members: 4, waitList});
  const open = async ({waitList, scope = short, deep = false, secrets = {invite: key}, way = 'invitation', own = {workspace: 'own-id', name: 'Ada'},
    reconnect = async ({workspace}) => ({joined: 'again', workspace})}) => {
    const steps = [];
    let card = null;
    const lounge = {joined: 'lounge', workspace: 'lounge-id', dispose: async () => steps.push('dispose lounge')};
    const {deps} = entries({
      openInvitation: async () => 'the invitation',
      confirmInvitation: async shown => { card = shown; return true; },
      inspectInvitation: async () => offer(waitList),
      joinWorkspace: async () => { steps.push('join lounge'); return lounge; },
      ownWorkspace: async peer => { steps.push(['own of', peer.workspace]); return own; },
      reconnect: async options => { steps.push(['enter', options.origin, options.workspace]); return reconnect(options); },
    });
    const landed = await admit({scope, deep, secrets, runtime: rules([way]).runtime, deps});
    return {landed, steps, lounge, card};
  };
  // Off the wait list: joined, then their own workspace made and entered,
  // with the joined one let go first, before anything is shown.
  const wait = await open({waitList: true});
  assert.deepEqual(wait.landed, {peer: {joined: 'again', workspace: 'own-id'}, own: true});
  assert.deepEqual(wait.steps, ['join lounge', ['own of', 'lounge-id'], 'dispose lounge', ['enter', server, 'own-id']]);
  assert.equal(wait.card, null, 'nothing is left to agree to: they asked to get in');
  // An ordinary invitation into the Lounge stays there.
  const plain = await open({waitList: false});
  assert.equal(plain.landed.peer, plain.lounge);
  assert.deepEqual(plain.steps, ['join lounge']);
  assert.equal(plain.card.workspaceName, 'Lounge', 'the card offers the Lounge itself');
  // The long link, which carries the invitation's own text, opens their own
  // workspace the same way.
  const named = await open({waitList: true, secrets: {invite: 'AAEC'}});
  assert.deepEqual(named.landed, {peer: {joined: 'again', workspace: 'own-id'}, own: true});
  // A link to a place inside the workspace (its chat, a message) opens that place.
  const deep = await open({waitList: true, deep: true, secrets: {invite: 'AAEC'}});
  assert.equal(deep.landed.peer, deep.lounge);
  assert.deepEqual(deep.steps, ['join lounge']);
  assert.equal(deep.card.ownFirst, undefined);
  // A server that keeps no workspace of their own leaves them where they were let in.
  const none = await open({waitList: true, own: null});
  assert.equal(none.landed.peer, none.lounge);
  // Their own workspace that does not let this device in: back where the invitation let them in.
  const refused = await open({waitList: true, reconnect: async ({workspace}) => (workspace === 'own-id' ? null : {joined: 'again', workspace})});
  assert.deepEqual(refused.landed, {peer: {joined: 'again', workspace: 'lounge-id'}});
});

test('a short link asks its workspace’s host for the invitation it keeps, and one taken back is never acted on', async () => {
  const key = 'A'.repeat(43);
  const {calls, deps} = entries();
  const way = () => rules(['invitation']).runtime;
  assert.deepEqual(await admit({scope: target, secrets: {invite: key}, runtime: way(), deps}), {peer: {joined: true}});
  assert.deepEqual(calls.map(([name]) => name), ['openInvitation', 'inspectInvitation', 'confirmInvitation', 'joinWorkspace']);
  assert.deepEqual(calls[0].slice(1), [target.host, target.workspace, key], 'the secret goes only to the host that keeps it');
  assert.equal(calls[1][1], target.host, 'the invitation is read at that host');
  assert.equal(calls[3][1].invitation, `opened ${key}`);
  // A host that keeps it no more (spent, lapsed or taken back), or a secret it
  // keeps none under, is never acted on: nothing is inspected, and the words
  // are the ones a spent invitation gets.
  const gone = entries({openInvitation: async () => { throw Object.assign(new Error('410'), {code: 'directory-error-invitation-gone'}); }});
  await assert.rejects(admit({scope: target, secrets: {invite: key}, runtime: way(), deps: gone.deps}), {code: 'invitation-unusable'});
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
    entries({...member, connectMember: async () => { throw new DeviceKeyError('device-key-missing', 'This account has no existing browser device'); }}),
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
  const busy = () => { throw new DeviceKeyError('device-key-busy', 'Every device key is held by another tab'); };
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
  await assert.rejects(admit({scope: target, runtime: stuck.runtime, deps: entries(member).deps}), {name: 'DeviceKeyError', code: 'device-key-busy'});
  assert.deepEqual(stuck.asked.at(-1).slice(0, 2), ['wayInAfter', 'reconnect']);
  // As where the workspace cannot admit a new device of the person by itself.
  const uninvited = rules(['accounts', 'reconnect']);
  await assert.rejects(admit({scope: target, runtime: uninvited.runtime, deps: entries({reconnect: async () => busy()}).deps}),
    {name: 'DeviceKeyError', code: 'device-key-busy'});
  assert.ok(!uninvited.asked.some(([, failed]) => failed === 'reconnect'), 'the runtime is not asked what follows');
});

test('a browser removed from its person’s devices is told so on every way in, and no other way is tried', async () => {
  // What a session reaches the page as when its server refused its hello and said why.
  const removed = () => Object.assign(new Error('This device was removed from your devices.'), {code: 'device-revoked'});
  // With a member's account here: the session stops as the page waits for it.
  const member = entries({readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => ({whenReady: async () => { throw removed(); }})});
  const asMember = rules(['accounts', 'member', 'entry']);
  await assert.rejects(admit({scope: target, runtime: asMember.runtime, deps: member.deps}), {code: 'device-revoked'});
  assert.ok(!asMember.asked.some(([name]) => name === 'wayInAfter'), 'the runtime is not asked what follows');
  assert.deepEqual(member.calls, [], 'no entry is asked');
  // Reconnecting: whether the session stops while connecting or as the page waits for it.
  for (const reconnect of [async () => { throw removed(); }, async () => ({whenReady: async () => { throw removed(); }})]) {
    const back = entries({workspaceAt: () => commons, reconnect});
    const way = rules(['reconnect', 'entry']);
    await assert.rejects(admit({scope: target, runtime: way.runtime, deps: back.deps}), {code: 'device-revoked'});
    assert.ok(!way.asked.some(([name]) => name === 'wayInAfter'), 'the runtime is not asked what follows');
    assert.deepEqual(back.calls, [], 'no entry is asked');
  }
  // Any other refusal of a member stays no access, whatever it says.
  const refused = entries({readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => ({whenReady: async () => { throw Object.assign(new Error('not admitted'), {code: 'unanswered'}); }})});
  await assert.rejects(admit({scope: target, runtime: rules(['accounts', 'member', 'no-access']).runtime, deps: refused.deps}),
    {name: 'AdmissionError', code: 'no-access'});
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
