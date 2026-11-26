import test from 'node:test';
import assert from 'node:assert/strict';
import {createIdentityActions} from './identity-actions.mjs';
import {openInvites} from './pairing.mjs';
import {pairWorkspaceDevice} from './workspace-pairing.mjs';

const person = 'ab'.repeat(32);
const invite = 'cd'.repeat(16);
const origin = 'https://host.example';
const workspace = '0e88f5c4-68e9-49ed-9340-6954947b66da';
const base = 'https://entry.example/';

test('pairing requires the root of the certified person and never creates a new one', async () => {
  for (const root of [null, {userPublicKey: person, sign() {}, locked: true},
    {userPublicKey: 'fe'.repeat(32), sign() {}}]) {
    const identity = async options => { assert.deepEqual(options, {create: false}); return root; };
    await assert.rejects(pairWorkspaceDevice({origin, workspace, person, base}, {
      identity, runtime: assert.fail, hostAt: assert.fail,
    }), /created your identity/);
  }
});

async function fixture() {
  const session = new AbortController();
  const root = {userPublicKey: person, sign() {}};
  const waiting = {digest: 'device', label: 'Tilo Assistant', pairingCode: '472131', invite};
  const fixture = {pending: [], answers: [], canReach: true};
  const host = {
    pending: async who => {
      assert.equal(who, root);
      if (!fixture.canReach) throw new Error('offline');
      return JSON.stringify(fixture.pending);
    },
    answer: async (who, digest, approved) => {
      assert.equal(who, root);
      fixture.answers.push([digest, approved]);
    },
  };
  const runtime = {formatLink(route, entry) {
    assert.deepEqual(JSON.parse(route), {mode: 'desktop', target: {scope: {kind: 'workspace', host: origin, workspace}}});
    assert.deepEqual(JSON.parse(entry), {base});
    return base + '#/@host.example/w/' + workspace;
  }};
  const card = () => ({status: {}, closed: false, close() { this.closed = true; }});
  await pairWorkspaceDevice({origin, workspace, person, base, signal: session.signal}, {
    identity: async () => root, runtime: async () => runtime,
    hostAt: async reached => { assert.equal(reached, origin); return host; },
    controller: options => createIdentityActions({...options, session: null,
      invites: openInvites({random: bytes => bytes.fill(0xcd)}),
      every: tick => { fixture.tick = tick; return () => { fixture.tick = null; }; }}),
    show: (_runtime, {link, cancel}) => (fixture.shown = {...card(), link, cancel}),
    confirm: (request, decide) => (fixture.confirm = {...card(), request, decide}),
  });
  fixture.poll = async () => { fixture.tick?.(); await new Promise(resolve => setTimeout(resolve, 0)); };
  return {...{session, waiting}, state: fixture};
}

test('the owner sees a recoverable network error and consciously answers only their current invite', async () => {
  const {state, waiting} = await fixture();
  assert.equal(state.shown.link, base + '#/@host.example/w/' + workspace + '?pair=' + person + invite);
  state.canReach = false;
  await state.poll();
  assert.equal(state.shown.status.hidden, false);
  assert.match(state.shown.status.textContent, /try again/);
  state.canReach = true;
  state.pending = [{...waiting, signIn: true}, {...waiting, invite: 'ea'.repeat(16)}];
  await state.poll();
  assert.equal(state.shown.status.hidden, true);
  assert.equal(state.confirm, undefined);
  state.pending.push(waiting);
  await state.poll();
  assert.equal(state.confirm.request.pairingCode, waiting.pairingCode);
  assert.deepEqual(state.answers, []);
  await state.confirm.decide(false);
  assert.deepEqual(state.answers, [['device', false]]);
  await state.poll();
  assert.equal(state.tick, null);
});

test('cancellation or losing the admitted session withdraws the link and stops approval', async () => {
  const first = await fixture();
  first.state.shown.cancel();
  first.state.pending = [first.waiting];
  await first.state.poll();
  assert.equal(first.state.confirm, undefined);
  assert.equal(first.state.shown.closed, true);
  const {session, state, waiting} = await fixture();
  state.pending = [waiting];
  await state.poll();
  session.abort();
  assert.equal(state.confirm.closed, true);
  await assert.rejects(state.confirm.decide(true), /expired/);
  assert.deepEqual(state.answers, []);
});
