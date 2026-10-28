import test from 'node:test';
import assert from 'node:assert/strict';
import {createNodeApps, nodeApplications} from './node-apps.mjs';

const ROOM = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const row = (resource, label, kind = 'applications.host-app', action = 'open') => ({
  provider: `workspace:${ROOM}`, app_id: 'so.tilo.shell', resource: {id: resource},
  action: {address: {resource, action}, resource_kind: kind, resource_label: label},
});

test('the applications the node opens are listed, Tilo ones with their package', () => {
  const apps = nodeApplications([
    row('app.host:org.libreoffice.LibreOffice.writer', 'LibreOffice Writer'),
    row('app.host:chromium-browser', 'Chromium'),
    row('app.so.tilo.terminal', 'Terminal', 'applications.app'),
    row('app.so.tilo.terminal', 'Terminal', 'applications.app', 'close'),
    row('app.host:chromium-browser', 'Chromium', 'applications.host-app', 'close'),
    row('app.', 'Nothing', 'applications.app'),
    row('so.tilo.notes', 'Notes', 'applications.app'),
    row('doc.1', 'A document', 'notes.note'),
    {provider: 'x'},
  ]);
  assert.deepEqual(apps.map(app => [app.id, app.title, app.app]), [
    ['app.host:chromium-browser', 'Chromium', undefined],
    ['app.host:org.libreoffice.LibreOffice.writer', 'LibreOffice Writer', undefined],
    ['app.so.tilo.terminal', 'Terminal', 'so.tilo.terminal'],
  ]);
  assert.deepEqual(apps[0].target, {provider: `workspace:${ROOM}`, address: {resource: 'app.host:chromium-browser', action: 'open'}});
});

test('the node session lists its apps and opens the one the person picks', async () => {
  const timers = [];
  const listed = [];
  const invoked = [];
  const queries = [];
  let pendingAsks = 0;
  let requests = [];
  const nodeApps = createNodeApps({
    room: ROOM,
    nodeSession: async () => 'ab'.repeat(32),
    actions: device => ({
      catalogue: async query => { queries.push({device, query}); return {rows: [row('app.host:chromium-browser', 'Chromium'), row('app.so.tilo.terminal', 'Terminal', 'applications.app')], exhausted: true}; },
      invoke: async target => { invoked.push({device, target}); return null; },
    }),
    setApps: apps => listed.push(apps),
    takeRequests: () => { const taken = requests; requests = []; return taken; },
    pending: () => { pendingAsks++; return requests.length > 0; },
    setInterval: (callback, ms) => { timers.push({callback, ms}); return timers.length; },
    clearInterval: () => {},
  });
  await nodeApps.refresh();
  // Only the catalogue refresh is on a timer: requests are pumped by the
  // page's own frame, so a hidden tab opens nothing and asks nothing.
  assert.deepEqual(timers.map(timer => timer.ms), [30_000]);
  assert.deepEqual(listed.at(-1), [
    {id: 'app.host:chromium-browser', title: 'Chromium'},
    {id: 'app.so.tilo.terminal', title: 'Terminal', app: 'so.tilo.terminal'},
  ]);
  assert.equal(queries[0].device, 'ab'.repeat(32));
  assert.equal(queries[0].query.provider, `workspace:${ROOM}`);
  // Nothing asked for: the frame's pump asks one predicate and drains nothing.
  nodeApps.pump();
  assert.deepEqual(invoked, []);
  assert.equal(pendingAsks, 1, 'one predicate, no drained list');
  assert.deepEqual(queries.length, 1, 'and no catalogue fetch either');
  requests = ['app.host:chromium-browser', 'app.host:unknown', 'app.so.tilo.terminal'];
  nodeApps.pump();
  assert.deepEqual(invoked, [
    {device: 'ab'.repeat(32), target: {provider: `workspace:${ROOM}`, address: {resource: 'app.host:chromium-browser', action: 'open'}}},
    {device: 'ab'.repeat(32), target: {provider: `workspace:${ROOM}`, address: {resource: 'app.so.tilo.terminal', action: 'open'}}},
  ]);
  nodeApps.close();
  assert.deepEqual(listed.at(-1), []);
});

test('a host that runs no session lists nothing', async () => {
  const listed = [];
  const nodeApps = createNodeApps({
    room: ROOM, nodeSession: async () => null, actions: () => { throw new Error('never asked'); },
    setApps: apps => listed.push(apps), takeRequests: () => [],
    setInterval: () => 0, clearInterval: () => {},
  });
  await nodeApps.refresh();
  assert.deepEqual(listed, [[]]);
  nodeApps.close();
});
