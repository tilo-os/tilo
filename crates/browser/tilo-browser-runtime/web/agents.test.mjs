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

/** Request timers the test runs out by hand: a request times out exactly
 * when `expire` says so, never because the runner was slow to answer. */
function manualTimers() {
  const pending = new Set();
  return {
    setTimer: callback => { const handle = {callback}; pending.add(handle); return handle; },
    clearTimer: handle => { pending.delete(handle); },
    /** Time out every request waiting now (once one is). */
    async expire() {
      while (!pending.size) await new Promise(resolve => setImmediate(resolve));
      for (const handle of [...pending]) { pending.delete(handle); handle.callback(); }
    },
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
  const time = manualTimers();
  const {agents, stop} = agentsWith(peer, time);
  try {
    await assert.rejects(agents.message('writer', 'more'), error => error instanceof AgentsError && error.code === 'refused' && /stopped/.test(error.message));
    const status = agents.status();
    await time.expire();
    await assert.rejects(status, error => error.code === 'timeout');
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
      {publisher: AGENT, seq: 3, focus: {app: 'so.tilo.scratchpad', record: 'r1'}, pointer: {window: {app: 'so.tilo.scratchpad', record: 'r1'}, at: [10, 20]}, selection: null, stale: false,
        participant: {kind: 'agent', label: 'Writer helper', color: '#7c3aed', activity: 'Edit note · Note'}},
      {publisher: 'ff'.repeat(32), seq: 9, pointer: {strip: '1', at: [1, 2]}, participant: {kind: 'person', label: 'Ada', color: '#0ea5e9', activity: null}},
      {publisher: '11'.repeat(32), seq: 1, pointer: {strip: '1', at: [5, 5]}, participant: null},
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
    focus: {app: 'so.tilo.scratchpad', record: 'r1'}, pointer: {window: {app: 'so.tilo.scratchpad', record: 'r1'}, at: [10, 20]}, selection: null, stale: false,
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

test('one task is cancelled by id without stopping the agent', async () => {
  const peer = fakePeer(request => {
    const {resource, action} = request.target.address;
    if (resource === 'agent:writer' && action === 'cancel-task') {
      return request.argument.Text === 't2' ? {Completed: {output: {value: {}}}} : {Rejected: {error: {Refused: {reason: 'the agent has no such task'}}}};
    }
    return {Rejected: {error: 'UnknownAction'}};
  });
  const {agents, stop} = agentsWith(peer);
  try {
    assert.equal(await agents.cancelTask('writer', 't2'), undefined);
    assert.deepEqual(peer.sent[0].frame.message.Invoke.target, {provider: 'agents', address: {resource: 'agent:writer', action: 'cancel-task'}});
    assert.deepEqual(peer.sent[0].frame.message.Invoke.argument, {Text: 't2'});
    await assert.rejects(agents.cancelTask('writer', 't9'), error => error instanceof AgentsError && error.code === 'refused' && /no such task/.test(error.message));
    await assert.rejects(agents.cancelTask('writer', ''), TypeError);
    assert.equal(peer.sent.length, 2, 'nothing is sent without a task id');
    assert.ok(!peer.sent.some(sent => sent.frame.message.Invoke.target.address.action === 'stop'));
  } finally {
    stop();
  }
});

test('a caller idempotency id names the invocation, so a resend is answered once', async () => {
  // The host's ledger: one answer per invocation id, as the supervisor keeps it.
  const answered = new Map();
  let tasks = 0;
  let silent = true;
  const peer = fakePeer(request => {
    if (silent) return undefined;
    const key = request.invocation;
    if (!answered.has(key)) {
      const {action} = request.target.address;
      tasks += 1;
      answered.set(key, {Completed: {output: {value: action === 'spawn' ? {agentId: 'writer', taskId: `t${tasks}`} : {taskId: `t${tasks}`}}}});
    }
    return answered.get(key);
  });
  const time = manualTimers();
  const {agents, stop} = agentsWith(peer, time);
  const spawnId = '0b5f1c2e-9d7a-4c55-8a1e-3f1d2c4b5a69';
  const messageId = '1C6F2D3E-0A8B-4D66-9B2F-4E2D3C5B6A70';
  try {
    // The first attempt goes unanswered; the caller resends under the same id.
    const unanswered = agents.spawn({grant: 'writer', task: 'tidy', id: spawnId});
    await time.expire();
    await assert.rejects(unanswered, error => error.code === 'timeout');
    silent = false;
    const spawned = await agents.spawn({grant: 'writer', task: 'tidy', id: spawnId});
    assert.deepEqual(await agents.spawn({grant: 'writer', task: 'tidy', id: spawnId}), spawned);
    assert.deepEqual(spawned, {agentId: 'writer', taskId: 't1'});
    assert.deepEqual(peer.sent.map(sent => sent.frame.message.Invoke.invocation), [spawnId, spawnId, spawnId]);

    // Two sends of one id while the first waits share one frame.
    const [first, second] = await Promise.all([
      agents.message('writer', 'add a heading', {id: messageId}),
      agents.message('writer', 'add a heading', {id: messageId}),
    ]);
    assert.deepEqual(first, {taskId: 't2'});
    assert.deepEqual(second, first);
    assert.equal(peer.sent.at(-1).frame.message.Invoke.invocation, messageId.toLowerCase());
    assert.equal(peer.sent.length, 4);
    assert.equal(tasks, 2, 'one agent and one task');

    await assert.rejects(agents.message('writer', 'x', {id: 'not-a-uuid'}), TypeError);
    await assert.rejects(agents.spawn({grant: 'writer', task: 'x', id: 7}), TypeError);
    assert.equal(peer.sent.length, 4);
    const fresh = await agents.message('writer', 'add a heading');
    assert.deepEqual(fresh, {taskId: 't3'}, 'without an id every call is new');
  } finally {
    stop();
  }
});


test('a pending invocation id cannot alias a different task or agent', async () => {
  const peer = fakePeer(() => ({Completed: {output: {value: {taskId: 't1'}}}}));
  const {agents, stop} = agentsWith(peer);
  const id = '37d1ca54-3c0f-4dfe-b4d5-e35f3d8ba8fd';
  try {
    const first = agents.message('writer', 'first task', {id});
    await assert.rejects(agents.message('writer', 'different task', {id}), error => error.code === 'invocation_conflict');
    await assert.rejects(agents.message('another-agent', 'first task', {id}), error => error.code === 'invocation_conflict');
    assert.deepEqual(await first, {taskId: 't1'});
    assert.equal(peer.sent.length, 1, 'only the original request is sent');
  } finally {
    stop();
  }
});


test('an invocation binding survives timeout and rejects different input', async () => {
  const peer = fakePeer(() => undefined);
  const time = manualTimers();
  const {agents, stop} = agentsWith(peer, time);
  const id = '46c880a1-55f3-4844-851d-3fb83f3c55b2';
  try {
    const original = agents.message('writer', 'original', {id});
    await time.expire();
    await assert.rejects(original, error => error.code === 'timeout');
    await assert.rejects(agents.message('writer', 'changed', {id}), error => error.code === 'invocation_conflict');
    assert.equal(peer.sent.length, 1);
  } finally { agents.dispose(); stop(); }
});

test('disposing an agent API permanently closes its transport', async () => {
  const peer = fakePeer(() => undefined);
  const {agents, stop} = agentsWith(peer, {timeoutMs: 10});
  try {
    agents.dispose();
    await assert.rejects(agents.cancelTask('writer', 'task'), error => error.code === 'closed');
    await assert.rejects(agents.status(), error => error.code === 'closed');
    assert.equal(peer.sent.length, 0);
  } finally { stop(); }
});
