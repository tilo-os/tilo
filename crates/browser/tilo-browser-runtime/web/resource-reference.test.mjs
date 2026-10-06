import test from 'node:test';
import assert from 'node:assert/strict';
import {createActions} from './actions.mjs';
import {resolveResourceReference} from './resource-reference.mjs';

const room = 'fixture-room', host = 'ab'.repeat(32), provider = 'record:so.tilo.chat:uuid:1';
const reference = {id: 'chat.message:m1', kind: 'chat.message', label: 'untrusted label'};
const resource = {...reference, label: 'Published message'};
const view = {address: {resource: resource.id, action: 'show'}, kind: 'View'};
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
    assert.equal(query.provider, provider); assert.equal(query.needle, reference.id);
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
