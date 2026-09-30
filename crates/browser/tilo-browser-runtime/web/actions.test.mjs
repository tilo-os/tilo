import test from 'node:test';
import assert from 'node:assert/strict';
import {ActionsError, confirmationFor, createActions, invokeArgument} from './actions.mjs';

const ROOM = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const HOST = 'ab'.repeat(32);
const PAGE = 'cd'.repeat(32);
const USER = 'ee'.repeat(32);
const SHEET = `record:so.tilo.sheets:uuid:1`;

const descriptor = (resource, action, label, confirmation = {mode: 'none'}) => ({
  address: {resource, action}, resource_kind: 'sheets.workbook', resource_label: 'Workbook', action_label: label,
  description: null, locus: 'ClientLocal', kind: 'Document', contract: {confirmation}, input: 'None', params: [],
});
const ROWS = [
  {provider: SHEET, app_id: 'so.tilo.sheets', parent_resource: '', resource: {id: 'sheets.workbook', kind: 'sheets.workbook', label: 'Workbook'},
    action: descriptor('sheets.workbook', 'new-sheet', 'New sheet'), has_children: false},
  {provider: SHEET, app_id: 'so.tilo.sheets', parent_resource: '', resource: {id: 'sheets.sheet:0', kind: 'sheets.sheet', label: 'Sheet1'},
    action: descriptor('sheets.sheet:0', 'clear', 'Clear sheet', {mode: 'confirm'}), has_children: false},
];

/**
 * A host with the room-actions ledger: one run per invocation id, a
 * conflict for a reused id, confirmation enforced, cancel before start.
 */
function fakeHost({hold = () => false} = {}) {
  const events = [];
  const sent = [];
  const ledger = new Map();
  const held = [];
  let runs = 0;
  const reply = message => events.push({kind: 'remoteAction', from: HOST, frame: JSON.stringify({version: 1, message})});
  const finish = request => {
    const entry = ledger.get(request.invocation);
    if (entry.update.Cancelled) return reply({Receipt: {invocation: request.invocation, status: {Cancelled: {message: 'cancelled before it started'}}}});
    runs += 1;
    entry.update = {Succeeded: {invocation: request.invocation, output: {value: {sheets: runs}, resources: []}}};
    reply({Receipt: {invocation: request.invocation, status: {Completed: {output: {value: {sheets: runs}, resources: []}}}}});
  };
  const peer = {
    sent,
    events,
    get runs() { return runs; },
    release() { for (const request of held.splice(0)) finish(request); },
    sendRemoteAction(room, to, frame) {
      assert.equal(room, ROOM);
      assert.equal(to, HOST);
      const message = JSON.parse(frame).message;
      sent.push(message);
      if (message.CatalogueRequest) {
        const {correlation, query} = message.CatalogueRequest;
        const rows = ROWS.filter(row => !query.needle || row.action.action_label.toLowerCase().includes(query.needle.toLowerCase()));
        reply({CatalogueResponse: {correlation, result: {Ok: {revision: '3', rows,
          providers: query.provider ? [] : [{provider: SHEET, app_id: 'so.tilo.sheets', room: ROOM, record_id: 'uuid:1', browsable: true}]}}}});
      } else if (message.Invoke) {
        const request = message.Invoke;
        const existing = ledger.get(request.invocation);
        if (existing) {
          if (JSON.stringify(existing.request) !== JSON.stringify(request)) {
            return reply({Receipt: {invocation: request.invocation, status: {Rejected: {error: 'InvocationConflict'}}}});
          }
          const update = existing.update;
          const status = update.Succeeded ? {Completed: {output: update.Succeeded.output}} : update.Cancelled ? {Cancelled: {message: null}} : 'Accepted';
          return reply({Receipt: {invocation: request.invocation, status}});
        }
        const row = ROWS.find(candidate => candidate.provider === request.target.provider
          && candidate.action.address.action === request.target.address.action);
        if (!row) return reply({Receipt: {invocation: request.invocation, status: {Rejected: {error: 'UnknownAction'}}}});
        if (row.action.contract.confirmation.mode === 'confirm' && request.confirmation !== 'Confirmed') {
          return reply({Receipt: {invocation: request.invocation, status: {Rejected: {error: 'ConfirmationRequired'}}}});
        }
        ledger.set(request.invocation, {request, update: {Accepted: {invocation: request.invocation}}});
        if (hold(request)) held.push(request); else finish(request);
      } else if (message.Observe) {
        const entry = ledger.get(message.Observe.invocation);
        reply({Update: {invocation: message.Observe.invocation, state: entry ? {update: entry.update} : null}});
      } else if (message.Cancel) {
        const {invocation} = message.Cancel;
        const entry = ledger.get(invocation);
        if (!entry) return reply({CancelResponse: {invocation, result: {Err: 'UnknownInvocation'}}});
        if (!entry.update.Accepted) return reply({CancelResponse: {invocation, result: {Err: {NotCancellable: {reason: 'the invocation has finished'}}}}});
        entry.update = {Cancelled: {invocation, message: 'cancelled before it started'}};
        reply({CancelResponse: {invocation, result: {Ok: null}}});
      }
    },
    takeRemoteActionEvents: () => JSON.stringify(events.splice(0)),
  };
  return peer;
}

function memoryStore() {
  const values = new Map();
  return {values, get: async key => values.get(key), put: async (key, value) => { values.set(key, value); }};
}

function actionsWith(peer, extra = {}) {
  let id = 0;
  const actions = createActions({peer, room: ROOM, hostDevice: HOST, device: PAGE, userPublicKey: USER,
    newId: () => `inv-${++id}`, ...extra});
  const pumping = setInterval(() => actions.pump(), 1);
  return {actions, stop: () => { clearInterval(pumping); actions.dispose(); }};
}

test('the catalogue lists the room and feeds the v1 capability', async () => {
  const peer = fakeHost();
  const {actions, stop} = actionsWith(peer);
  try {
    assert.equal(actions.v1.getSnapshot().capability.reason, 'not-discovered');
    const page = await actions.catalogue();
    assert.equal(page.providers[0].provider, SHEET);
    const ready = await actions.v1.catalogue();
    assert.equal(ready.state, 'ready');
    assert.equal(ready.host, HOST);
    assert.deepEqual(ready.binding, {room: ROOM, sender: PAGE, senderRoot: USER, target: HOST, targetRoot: USER, certificate: HOST});
    const [surface] = ready.catalogue.surfaces;
    assert.equal(surface.peer_id, SHEET);
    assert.equal(surface.record_id, 'uuid:1');
    assert.equal(surface.cluster_id, ROOM);
    assert.deepEqual(surface.actions.map(action => [action.resource, action.action, action.label]),
      [['sheets.workbook', 'new-sheet', 'New sheet'], ['sheets.sheet:0', 'clear', 'Clear sheet']]);
    assert.deepEqual(surface.actions[1].contract.confirmation, {mode: 'confirm'});
  } finally { stop(); }
});

test('watching the v1 capability discovers the catalogue, and a host without actions says so', async () => {
  const {actions, stop} = actionsWith(fakeHost());
  try {
    const ready = await new Promise(resolve => actions.v1.subscribe(snapshot => {
      if (snapshot.capability.state === 'ready') resolve(snapshot);
    }));
    assert.equal(ready.capability.catalogue.surfaces[0].peer_id, SHEET);
  } finally { stop(); }
  const silent = {sendRemoteAction() {}, takeRemoteActionEvents: () => '[]'};
  const quiet = actionsWith(silent, {timeoutMs: 10});
  try {
    const unavailable = await new Promise(resolve => quiet.actions.v1.subscribe(snapshot => {
      if (snapshot.capability.state === 'unavailable' && snapshot.capability.reason !== 'not-discovered') resolve(snapshot);
    }));
    assert.equal(unavailable.capability.reason, 'timeout');
  } finally { quiet.stop(); }
});

test('an invocation keeps its id: a retry observes the original and runs once', async () => {
  const peer = fakeHost();
  const store = memoryStore();
  const {actions, stop} = actionsWith(peer, {store});
  try {
    await actions.loaded;
    const target = {provider: SHEET, address: {resource: 'sheets.workbook', action: 'new-sheet'}};
    const ticket = actions.invoke(target, 'Budget', {label: 'New sheet'});
    const done = await ticket.done;
    assert.equal(done.state, 'succeeded');
    assert.deepEqual(done.update.output.value, {sheets: 1});
    // The intent was saved before it was sent.
    const saved = JSON.parse(store.values.get(`tilo.actions.v2:${ROOM}`));
    assert.equal(saved.at(-1).invocation, ticket.id);
    assert.deepEqual(saved.at(-1).argument, {Text: 'Budget'});
    // An unknown outcome is retried under the same id: the host answers
    // with the original receipt and nothing runs again.
    await actions.v1.invoke({app_peer: SHEET, app_id: 'so.tilo.sheets', record_id: 'uuid:1', resource: 'sheets.workbook',
      action: 'new-sheet', revision: null, invocation: ticket.id, argument: 'Budget'});
    assert.equal(peer.runs, 1);
    const [entry] = actions.v1.getSnapshot().entries;
    assert.equal(entry.invocation, ticket.id);
    assert.equal(entry.terminal, true);
    assert.equal(entry.intent.app_peer, SHEET);
    assert.equal(entry.intent.record_id, 'uuid:1');
    assert.equal(entry.intent.argument, 'Budget');
    // A reload reads the history back.
    const reloaded = createActions({peer, room: ROOM, hostDevice: HOST, store});
    await reloaded.loaded;
    assert.equal(reloaded.history()[0].invocation, ticket.id);
    reloaded.dispose();
  } finally { stop(); }
});

test('a lost receipt leaves the outcome unknown until status observes the original', async () => {
  const peer = fakeHost();
  let dropped = false;
  const lossy = {sendRemoteAction: (...args) => peer.sendRemoteAction(...args), takeRemoteActionEvents() {
    const events = JSON.parse(peer.takeRemoteActionEvents());
    if (!dropped && events.length) { dropped = true; return '[]'; }
    return JSON.stringify(events);
  }};
  const {actions, stop} = actionsWith(lossy, {timeoutMs: 20});
  try {
    const ticket = actions.invoke({provider: SHEET, address: {resource: 'sheets.workbook', action: 'new-sheet'}}, 'Budget');
    await assert.rejects(ticket.done, error => error.code === 'timeout');
    assert.equal(actions.history()[0].state, 'unknown');
    const after = await actions.status(ticket.id);
    assert.equal(after.state, 'succeeded');
    assert.equal(peer.runs, 1);
    assert.equal(peer.sent.filter(message => message.Invoke).length, 1, 'observing never reruns');
  } finally { stop(); }
});

test('a request the host never received is resent under the same id', async () => {
  const peer = fakeHost();
  let lost = false;
  const lossy = {takeRemoteActionEvents: () => peer.takeRemoteActionEvents(), sendRemoteAction(room, to, frame) {
    if (!lost && JSON.parse(frame).message.Invoke) { lost = true; return; }
    peer.sendRemoteAction(room, to, frame);
  }};
  const {actions, stop} = actionsWith(lossy, {timeoutMs: 20});
  try {
    const ticket = actions.invoke({provider: SHEET, address: {resource: 'sheets.workbook', action: 'new-sheet'}}, 'Budget');
    await assert.rejects(ticket.done, error => error.code === 'timeout');
    const recovered = await actions.recover();
    const [entry] = recovered.entries;
    assert.equal(entry.invocation, ticket.id);
    assert.equal(entry.state, 'succeeded');
    assert.equal(peer.runs, 1);
    const invokes = peer.sent.filter(message => message.Invoke);
    assert.deepEqual(invokes.map(message => message.Invoke.invocation), [ticket.id]);
  } finally { stop(); }
});

test('a destructive action needs the person to confirm it', async () => {
  const peer = fakeHost();
  const {actions, stop} = actionsWith(peer);
  try {
    const clear = {provider: SHEET, address: {resource: 'sheets.sheet:0', action: 'clear'}};
    assert.deepEqual(confirmationFor(ROWS[1].action), {kind: 'confirm'});
    assert.equal(confirmationFor(ROWS[0].action), null);
    await assert.rejects(actions.invoke(clear).done, error => error.code === 'action_confirmation_required');
    assert.equal(peer.runs, 0);
    const confirmed = await actions.invoke(clear, undefined, {confirmation: {kind: 'confirm'}}).done;
    assert.equal(confirmed.state, 'succeeded');
    assert.equal(peer.sent.at(-1).Invoke.confirmation, 'Confirmed');
    // The palette refuses to run it unconfirmed without asking the host.
    const [hit] = await actions.provider.query('clear');
    assert.deepEqual(hit.target.confirmation, {kind: 'confirm'});
    assert.throws(() => actions.provider.choose(hit), ActionsError);
    const chosen = await actions.provider.choose(hit, {confirmation: {kind: 'confirm'}}).done;
    assert.equal(chosen.state, 'succeeded');
  } finally { stop(); }
});

test('a ticket is cancelled before it starts; a finished one is not cancellable', async () => {
  const peer = fakeHost({hold: request => request.argument?.Text === 'Later'});
  const {actions, stop} = actionsWith(peer);
  try {
    const target = {provider: SHEET, address: {resource: 'sheets.workbook', action: 'new-sheet'}};
    const later = actions.invoke(target, 'Later');
    await new Promise(resolve => setTimeout(resolve, 10));
    const cancelled = await later.cancel();
    assert.equal(cancelled.cancellationRequested, true);
    assert.equal(cancelled.state, 'cancelled');
    peer.release();
    const outcome = await later.done;
    assert.equal(outcome.state, 'cancelled');
    assert.equal(peer.runs, 0);
    const now = await actions.invoke(target, 'Now').done;
    assert.equal(now.state, 'succeeded');
    const entry = await actions.v1.cancel(now.invocation);
    assert.equal(entry.state, 'succeeded', 'a finished invocation is reported as it is');
    // Reusing a finished id reports it; nothing is sent or run again.
    const sent = peer.sent.length;
    const reused = await actions.invoke(target, 'Other', {invocation: now.invocation}).done;
    assert.equal(reused.invocation, now.invocation);
    assert.equal(peer.sent.length, sent);
    assert.equal(peer.runs, 1);
  } finally { stop(); }
});

test('arguments and palette hits use the remote vocabulary', async () => {
  assert.equal(invokeArgument(undefined), 'None');
  assert.deepEqual(invokeArgument('x'), {Text: 'x'});
  assert.deepEqual(invokeArgument({n: 2}), {Json: {n: 2}});
  const peer = fakeHost();
  const {actions, stop} = actionsWith(peer);
  try {
    assert.deepEqual(await actions.provider.query(''), []);
    const hits = await actions.provider.query('new');
    assert.equal(hits.length, 1);
    assert.equal(hits[0].kind, 'Action');
    assert.equal(hits[0].provider, 'room-actions');
    assert.equal(hits[0].title, 'New sheet');
    assert.deepEqual(hits[0].target.address, {resource: 'sheets.workbook', action: 'new-sheet'});
    const snapshots = [];
    const unsubscribe = actions.v1.subscribe(snapshot => snapshots.push(snapshot));
    await actions.provider.choose(hits[0], {argument: 'Q3'}).done;
    unsubscribe();
    assert.ok(snapshots.some(snapshot => snapshot.entries.some(entry => entry.state === 'succeeded')));
  } finally { stop(); }
});
