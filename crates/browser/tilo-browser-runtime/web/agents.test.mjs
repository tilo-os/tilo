import test from 'node:test';
import assert from 'node:assert/strict';
import {AgentsError, createAgents} from './agents.mjs';

const ROOM = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const HOST = 'ab'.repeat(32);
const AGENT = 'cd'.repeat(32);
const USER = 'ee'.repeat(32);

/** A peer whose host answers every invoke with `answer(request)`. */
function fakePeer(answer) {
  const events = [];
  const sent = [];
  return {
    sent,
    events,
    sendRemoteAction(room, to, frame) {
      sent.push({room, to, frame: JSON.parse(frame)});
      const request = JSON.parse(frame).message.Invoke;
      const status = answer(request);
      if (status === undefined) return;
      events.push({kind: 'remoteAction', from: to, frame: JSON.stringify({version: 1, message: {Receipt: {invocation: request.invocation, status}}})});
    },
    takeRemoteActionEvents: () => JSON.stringify(events.splice(0)),
  };
}

function agentsWith(peer, extra = {}) {
  let id = 0;
  const agents = createAgents({peer, room: ROOM, hostDevice: HOST, userPublicKey: USER, newId: () => `inv-${++id}`, ...extra});
  const pumping = setInterval(() => agents.pump(), 1);
  return {agents, stop: () => clearInterval(pumping)};
}

test('agent control speaks tilo_actions remote frames to the host device', async () => {
  const peer = fakePeer(request => {
    const {resource, action} = request.target.address;
    if (resource === 'agents' && action === 'status') return {Completed: {output: {value: {version: 1, agents: [], grants: [{id: 'writer'}]}}}};
    if (resource === 'agents' && action === 'spawn') return {Completed: {output: {value: {agentId: request.argument.Json.grant, taskId: 't1'}}}};
    if (action === 'message') return {Completed: {output: {value: {taskId: 't2'}}}};
    if (action === 'pause' || action === 'resume' || action === 'stop') return {Completed: {output: {value: {}}}};
    return {Rejected: {error: 'UnknownAction'}};
  });
  const {agents, stop} = agentsWith(peer);
  try {
    assert.deepEqual((await agents.status()).grants, [{id: 'writer'}]);
    assert.deepEqual(await agents.spawn({grant: 'writer', task: 'tidy the notes'}), {agentId: 'writer', taskId: 't1'});
    assert.deepEqual(await agents.message('writer', 'and add a heading'), {taskId: 't2'});
    await agents.pause('writer');
    await agents.resume('writer');
    await agents.stop('writer');
    const [status, spawn, message, pause] = peer.sent;
    assert.equal(status.to, HOST);
    assert.equal(status.room, ROOM);
    assert.deepEqual(status.frame.message.Invoke.target, {provider: 'agents', address: {resource: 'agents', action: 'status'}});
    assert.equal(status.frame.message.Invoke.argument, 'None');
    assert.deepEqual(spawn.frame.message.Invoke.argument, {Json: {grant: 'writer', task: 'tidy the notes'}});
    assert.deepEqual(message.frame.message.Invoke.target.address, {resource: 'agent:writer', action: 'message'});
    assert.deepEqual(message.frame.message.Invoke.argument, {Text: 'and add a heading'});
    assert.equal(pause.frame.message.Invoke.target.address.action, 'pause');
    assert.equal(new Set(peer.sent.map(sent => sent.frame.message.Invoke.invocation)).size, peer.sent.length, 'every call has its own invocation id');
  } finally {
    stop();
  }
});

test('grants are requested from the host and revoked by id', async () => {
  const peer = fakePeer(request => {
    const {resource, action} = request.target.address;
    if (resource === 'agents' && action === 'request-grant') return {Completed: {output: {value: {grant: 'writer-0a1b2c', pairingCode: '123456'}}}};
    if (action === 'revoke') return {Completed: {output: {value: {}}}};
    if (action === 'spawn') return {Completed: {output: {value: {agentId: request.argument.Json.grant, taskId: 't1'}}}};
    return {Rejected: {error: 'UnknownAction'}};
  });
  const {agents, stop} = agentsWith(peer);
  try {
    assert.deepEqual(await agents.requestGrant({label: 'Writer', rooms: [ROOM], apps: ['so.tilo.notes'], expiresInSecs: 3600}),
      {grant: 'writer-0a1b2c', pairingCode: '123456'});
    await agents.spawn({grant: 'writer-0a1b2c', task: 'Create a new note titled "Groceries"', room: ROOM});
    await agents.revoke('writer-0a1b2c');
    const [grant, spawn, revoke] = peer.sent;
    assert.deepEqual(grant.frame.message.Invoke.target.address, {resource: 'agents', action: 'request-grant'});
    assert.deepEqual(grant.frame.message.Invoke.argument,
      {Json: {label: 'Writer', rooms: [ROOM], apps: ['so.tilo.notes'], expiresInSecs: 3600, mayConfirm: false}});
    assert.equal(spawn.frame.message.Invoke.argument.Json.room, ROOM);
    assert.deepEqual(revoke.frame.message.Invoke.target.address, {resource: 'agent:writer-0a1b2c', action: 'revoke'});
  } finally {
    stop();
  }
});

test('refusals, undeliverable frames and silence become typed errors', async () => {
  const peer = fakePeer(request => (request.target.address.action === 'message'
    ? {Rejected: {error: {Refused: {reason: 'the agent was stopped'}}}}
    : undefined));
  const timers = [];
  const {agents, stop} = agentsWith(peer, {
    timeoutMs: 5,
    setTimer: (callback, delay) => { const handle = setTimeout(callback, delay); timers.push(handle); return handle; },
  });
  try {
    await assert.rejects(agents.message('writer', 'more'), error => error instanceof AgentsError && error.code === 'refused' && /stopped/.test(error.message));
    await assert.rejects(agents.status(), error => error.code === 'timeout');
    const waiting = agents.pause('writer');
    peer.events.push({kind: 'undeliverable', to: HOST, reason: 'this host does not serve apps'});
    await assert.rejects(waiting, error => error.code === 'undeliverable');
  } finally {
    stop();
  }
});

test('agent cursors come from presence that says agent; delegations verify in wasm', () => {
  const live = {
    remoteState: () => [
      {publisher: AGENT, seq: 3, focus: {app: 'so.tilo.scratchpad', record: 'r1'}, pointer: [10, 20], selection: null, stale: false,
        participant: {kind: 'agent', label: 'Writer helper', color: '#7c3aed', activity: 'Edit note · Note'}},
      {publisher: 'ff'.repeat(32), seq: 9, pointer: [1, 2], participant: {kind: 'person', label: 'Ada', color: '#0ea5e9', activity: null}},
      {publisher: '11'.repeat(32), seq: 1, pointer: [5, 5], participant: null},
    ],
  };
  const calls = [];
  const runtime = {
    verifyAgentDelegation(delegation, user, nowMs) {
      calls.push({delegation, user, nowMs});
      if (delegation !== 'good') throw new Error('signature');
      return JSON.stringify({agentId: 'writer', device: AGENT, label: 'Writer helper', rooms: [ROOM], apps: []});
    },
  };
  const agents = createAgents({peer: fakePeer(() => undefined), runtime, room: ROOM, hostDevice: HOST, userPublicKey: USER, live, wallNow: () => 42});
  assert.deepEqual(agents.cursors(), [{
    device: AGENT, label: 'Writer helper', color: '#7c3aed', activity: 'Edit note · Note',
    focus: {app: 'so.tilo.scratchpad', record: 'r1'}, pointer: [10, 20], selection: null, stale: false,
  }]);
  assert.equal(agents.verify({id: 'writer', device: AGENT, delegation: 'good'}).agentId, 'writer');
  assert.deepEqual(calls[0], {delegation: 'good', user: USER, nowMs: 42});
  assert.equal(agents.verify({id: 'writer', device: AGENT, delegation: 'forged'}), null);
  assert.equal(agents.verify({id: 'writer', device: 'ff'.repeat(32), delegation: 'good'}), null, 'a delegation of another device is not this agent');
});

test('actionsV1 drives an agent\'s open records over frames to its device', async () => {
  const events = [];
  const sent = [];
  const peer = {
    sendRemoteAction(room, to, frame) {
      const message = JSON.parse(frame).message;
      sent.push({room, to, message});
      let answer;
      if (message.CatalogueRequest) {
        answer = {CatalogueResponse: {correlation: message.CatalogueRequest.correlation, result: {Ok: {revision: '3', rows: [
          {provider: 'record:so.tilo.scratchpad:uuid:1', app_id: 'so.tilo.scratchpad', resource: {id: 'scratchpad.note', kind: 'note', label: 'Note'},
            action: {address: {resource: 'scratchpad.note', action: 'edit'}}},
        ]}}}};
      } else if (message.Invoke.target.provider.startsWith('record:')) {
        answer = {Receipt: {invocation: message.Invoke.invocation, status: {Completed: {output: {value: 'done', resources: ['r']}}}}};
      } else {
        answer = {Receipt: {invocation: message.Invoke.invocation, status: {Rejected: {error: 'UnknownAction'}}}};
      }
      // A reply from another device never settles this device's request.
      events.push({kind: 'remoteAction', from: HOST, frame: JSON.stringify({version: 1, message: answer})});
      events.push({kind: 'remoteAction', from: to, frame: JSON.stringify({version: 1, message: answer})});
    },
    takeRemoteActionEvents: () => JSON.stringify(events.splice(0)),
  };
  const {agents, stop} = agentsWith(peer);
  try {
    const actions = agents.actions(AGENT);
    const page = await actions.catalogue({needle: 'edit'});
    const row = page.rows[0];
    assert.equal(sent[0].to, AGENT);
    assert.equal(sent[0].message.CatalogueRequest.query.needle, 'edit');
    const target = {provider: row.provider, address: row.action.address};
    assert.deepEqual(await actions.invoke(target, {Json: {text: 'hi'}}, {confirm: true}), {value: 'done', resources: ['r']});
    assert.equal(sent[1].message.Invoke.confirmation, 'Confirmed');
    assert.deepEqual(sent[1].message.Invoke.argument, {Json: {text: 'hi'}});
    await assert.rejects(actions.invoke({provider: 'workspace:x', address: {resource: 'app.nope', action: 'open'}}),
      error => error.code === 'unknown_action');
  } finally {
    stop();
  }
});
