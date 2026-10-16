import test from 'node:test';
import assert from 'node:assert/strict';
import {createActions} from './actions.mjs';
import {resolveResourceReference, openResourceReference} from './resource-reference.mjs';

const room = 'fixture-room', host = 'ab'.repeat(32), provider = 'record:so.tilo.chat:uuid:1';
const reference = {id: 'chat.message:m1', kind: 'chat.message', label: 'untrusted label'};
const resource = {...reference, label: 'Published message'};
const view = {address: {resource: resource.id, action: 'show'}, kind: 'View',
  view: {id: 'resource', label: 'Resource', open_action: 'show', primary: true}};
const row = {provider, app_id: 'so.tilo.chat', resource, action: view};

// Two endpoints exchange the actual remote action frames. Authentication and
// room admission remain the existing CertifiedPeer's job, not this resolver's.
function endpoints(reply) {
  const received = [], sent = [];
  const peer = {
    sendRemoteAction(toRoom, toHost, frame) {
      assert.equal(toRoom, room); assert.equal(toHost, host);
      const message = JSON.parse(frame).message; sent.push(message);
      assert.ok(message.CatalogueRequest, 'resolution never invokes an action');
      const {correlation, query} = message.CatalogueRequest;
      received.push({kind: 'remoteAction', from: host, frame: JSON.stringify({version: 1,
        message: {CatalogueResponse: {correlation, result: {Ok: reply(query)}}}})});
      queueMicrotask(() => actions.pump());
    },
    takeRemoteActionEvents: () => JSON.stringify(received.splice(0)),
  };
  const actions = createActions({peer, room, hostDevice: host});
  return {actions, sent};
}

test('shared resource resolves only through its admitted record and current publisher', async () => {
  const {actions, sent} = endpoints(query => {
    assert.equal(query.provider, provider); assert.deepEqual(query.view_resource, {id: reference.id, kind: reference.kind});
    assert.equal(query.needle, undefined);
    assert.equal(query.max_results, 256);
    return {revision: 'r1', rows: [row], providers: []};
  });
  try {
    const resolved = await resolveResourceReference({reference, provider, actions, current: () => true});
    assert.equal(resolved.resource.label, 'Published message');
    assert.equal(resolved.app, 'so.tilo.chat');
    assert.deepEqual(resolved.views, [view]);
    assert.equal(sent.length, 1);
  } finally { actions.dispose(); }
});

test('revocation, wrong records and bounded lookup failure never resolve or invoke', async () => {
  for (const page of [
    {revision: 'r2', rows: []},
    {revision: 'r2', rows: [{...row, provider: 'record:so.tilo.chat:other'}]},
    {revision: 'r2', rows: [{...row, resource: {...resource, kind: 'files.entry'}}]},
    {revision: 'r2', rows: [row], next_cursor: [1]},
  ]) {
    const {actions, sent} = endpoints(() => page);
    try {
      await assert.rejects(resolveResourceReference({reference, provider, actions, current: () => true}),
        /unavailable|budget/);
      assert.equal(sent.length, 1);
    } finally { actions.dispose(); }
  }
});

test('disconnect and replacement session fence replies; consent is never implicit', async () => {
  let admitted = false;
  const {actions, sent} = endpoints(() => { admitted = false; return {revision: 'r1', rows: [row]}; });
  try {
    const resolve = () => resolveResourceReference({reference, provider, actions, current: () => admitted});
    await assert.rejects(resolve(), /current admitted room/);
    assert.equal(sent.length, 0);
    admitted = true;
    await assert.rejects(resolve(), /connection changed/);
    assert.equal(sent.length, 1);
    await assert.rejects(resolveResourceReference({reference, provider: 'workspace:other', actions,
      current: () => true}), /Invalid shared resource/);
    assert.equal(sent.length, 1);
  } finally { actions.dispose(); }
});


test('explicit browser opening hydrates the exact record and invokes only its local view door', async () => {
  const {actions, sent} = endpoints(() => ({revision: 'r1', rows: [row]}));
  const steps = [];
  const opened = {actions: id => { steps.push(['actions', id]); return [view]; },
    invokeAction: (...args) => { steps.push(['view', ...args]); return {state: 'completed'}; },
    cleanup: async () => steps.push(['cleanup'])};
  try {
    const result = await openResourceReference({reference, provider, action: 'show', actions,
      current: () => true, waitRecord: async (...args) => steps.push(['hydrate', ...args]),
      mount: async config => { steps.push(['mount', config]); return opened; }});
    assert.equal(result, opened);
    assert.deepEqual(steps, [['hydrate', 'so.tilo.chat', 'uuid:1'],
      ['mount', {app: 'so.tilo.chat', record: 'uuid:1'}], ['actions', reference.id],
      ['view', reference.id, 'show']]);
    assert.equal(sent.length, 2, 'the host is checked again after hydration; navigation runs locally');
  } finally { actions.dispose(); }
});

test('revoked, closed and disconnected views clean up without invoking or changing documents', async () => {
  for (const interrupted of ['undeclared', 'missing', 'document', 'input', 'disconnected', 'refused']) {
    const {actions} = endpoints(() => ({revision: 'r1', rows: [row]}));
    let connected = true, cleaned = 0, invoked = 0;
    const opened = {actions: () => interrupted === 'missing' ? [] : [{...view,
      kind: interrupted === 'document' ? 'Document' : 'View',
      view: interrupted === 'undeclared' ? null : view.view,
      input: interrupted === 'input' ? 'Text' : 'None'}],
      invokeAction: () => { invoked += 1; throw Error('permission revoked'); },
      cleanup: async () => { cleaned += 1; }};
    try {
      await assert.rejects(openResourceReference({reference, provider, action: 'show', actions,
        current: () => connected, waitRecord: async () => {},
        mount: async () => { if (interrupted === 'disconnected') connected = false; return opened; }}),
      /unavailable|connection changed|permission revoked/);
      assert.equal(cleaned, 1);
      assert.equal(invoked, interrupted === 'refused' ? 1 : 0);
    } finally { actions.dispose(); }
  }
});


test('package-primary resources mount the canonical existing record; ranges never silently degrade', async () => {
  const source = 'record:so.tilo.chat:';
  const {actions, sent} = endpoints(() => ({revision: 'r1', rows: [{...row, provider: source}]}));
  const steps = [];
  try {
    await openResourceReference({reference, provider: source, action: 'show', actions,
      current: () => true, waitRecord: async (...args) => steps.push(args),
      mount: async config => { steps.push(config); return {actions: () => [view],
        invokeAction: () => ({state: 'completed'}), cleanup: async () => {}}; }});
    assert.deepEqual(steps, [['so.tilo.chat', ''], {app: 'so.tilo.chat', record: '@tilo:package-primary'}]);
    await assert.rejects(openResourceReference({reference: {...reference, span: {start: 1, end: 2, unit: 'Chars'}},
      provider: source, action: 'show', actions, current: () => true}), /range-aware/);
    assert.equal(sent.length, 3);
  } finally { actions.dispose(); }
});


test('cancelling a pending browser mount cleans it up before invoking the resource', async () => {
  const {actions} = endpoints(() => ({revision: 'r1', rows: [row]}));
  const controller = new AbortController();
  let cleaned = 0;
  try {
    await assert.rejects(openResourceReference({reference, provider, action: 'show', actions,
      signal: controller.signal, current: () => true, waitRecord: async () => {},
      mount: async () => { controller.abort(); return {cleanup: async () => { cleaned += 1; },
        actions: () => { throw Error('cancelled navigation must not read the mounted surface'); }}; }}),
      error => error.name === 'AbortError');
    assert.equal(cleaned, 1);
  } finally { actions.dispose(); }
});

test('read queries without a declared navigation view cannot open a resource', async () => {
  const {actions} = endpoints(() => ({revision: 'r1', rows: [{...row,
    action: {...view, view: null}}]}));
  try {
    await assert.rejects(openResourceReference({reference, provider, action: 'show', actions,
      current: () => true, waitRecord: () => assert.fail('must not hydrate'),
      mount: () => assert.fail('must not mount')}), /view unavailable/);
  } finally { actions.dispose(); }
});

test('optional embedded-view metadata does not replace the declared ordinary open action', async () => {
  const embedded = {...view, view: {...view.view, mount: {action: 'embed',
    capability_param: 'capability', view_param: 'view'}}};
  const {actions} = endpoints(() => ({revision: 'r1', rows: [{...row, action: embedded}]}));
  let invoked = 0;
  try {
    const opened = {actions: () => [embedded], cleanup: async () => {},
      invokeAction: (id, action) => {
        assert.equal(id, reference.id); assert.equal(action, 'show'); invoked += 1;
        return {state: 'completed'};
      }};
    assert.equal(await openResourceReference({reference, provider, action: 'show', actions,
      current: () => true, waitRecord: async () => {}, mount: async () => opened}), opened);
    assert.equal(invoked, 1);
  } finally { actions.dispose(); }
});

test('range navigation retains units and invokes only the declared local range action', async () => {
  const range = {action: 'range', start_param: 'start', end_param: 'end', unit_param: 'unit'};
  const ranged = {...view, view: {...view.view, range}};
  const target = {address: {...view.address, action: 'range'}, kind: 'View', input: 'None',
    params: [{name: 'start', ty: 'integer', required: true}, {name: 'end', ty: 'integer', required: true},
      {name: 'unit', ty: 'text', required: true}]};
  for (const unit of ['Chars', 'Bytes']) {
    const {actions} = endpoints(() => ({revision: 'r', rows: [{...row, action: ranged}, {...row, action: target}]}));
    const calls = [];
    try {
      await openResourceReference({reference: {...reference, span: {start: 1, end: 3, unit}}, provider,
        action: 'show', actions, current: () => true, waitRecord: async () => {},
        mount: async () => ({actions: () => [ranged, target], cleanup: async () => {},
          invokeAction: (...args) => { calls.push(args); return {state: 'completed'}; }})});
      assert.deepEqual(calls, [[reference.id, 'range', {start: 1, end: 3, unit}]]);
    } finally { actions.dispose(); }
  }
});

test('invalid and changed range contracts refuse navigation and clean provisional mounts', async () => {
  const range = {action: 'range', start_param: 'start', end_param: 'end', unit_param: 'unit'};
  const ranged = {...view, view: {...view.view, range}};
  const {actions} = endpoints(() => ({revision: 'r', rows: [{...row, action: ranged}, {...row, action: {address: {...view.address, action: 'range'}, kind: 'View'}}]}));
  let cleaned = 0;
  try {
    for (const span of [{start: -1, end: 2, unit: 'Chars'}, {start: 3, end: 2, unit: 'Bytes'},
      {start: 0, end: Number.MAX_SAFE_INTEGER + 1, unit: 'Chars'}, {start: 0, end: 1, unit: 'Pixels'}]) {
      await assert.rejects(openResourceReference({reference: {...reference, span}, provider,
        action: 'show', actions, current: () => true}), /Invalid.*range/);
    }
    await assert.rejects(openResourceReference({reference: {...reference, span: {start: 0, end: 1, unit: 'Chars'}},
      provider, action: 'show', actions, current: () => true, waitRecord: async () => {},
      mount: async () => ({actions: () => [{...ranged, view: {...view.view, range: {...range, action: 'other'}}}],
        cleanup: async () => { cleaned++; }, invokeAction: () => assert.fail('changed range must not run')})}), /range view changed/);
    assert.equal(cleaned, 1);
  } finally { actions.dispose(); }
});

test('access revoked while hydrating refuses cached local views before navigation', async () => {
  let reads = 0, cleaned = 0;
  const {actions} = endpoints(() => ({revision: `r${++reads}`, rows: reads === 1 ? [row] : []}));
  try {
    await assert.rejects(openResourceReference({reference, provider, action: 'show', actions,
      current: () => true, waitRecord: async () => {}, mount: async () => ({actions: () => [view],
        invokeAction: () => assert.fail('revoked cached resource must not open'),
        cleanup: async () => { cleaned++; }})}), /not permitted/);
    assert.equal(reads, 2); assert.equal(cleaned, 1);
  } finally { actions.dispose(); }
});
