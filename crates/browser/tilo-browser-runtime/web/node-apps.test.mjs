import test from 'node:test';
import assert from 'node:assert/strict';
import {createNodeApps, nodeApplications} from './node-apps.mjs';

const ROOM = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
const row = (resource, label, kind = 'applications.host-app', action = 'open') => ({
  provider: `workspace:${ROOM}`, app_id: 'so.tilo.shell', resource: {id: resource},
  action: {address: {resource, action}, resource_kind: kind, resource_label: label},
});

test('only the unconfined applications the node opens are listed', () => {
  const apps = nodeApplications([
    row('app.host:org.libreoffice.LibreOffice.writer', 'LibreOffice Writer'),
    row('app.host:chromium-browser', 'Chromium'),
    row('app.so.tilo.notes', 'Notes', 'applications.app'),
    row('app.host:chromium-browser', 'Chromium', 'applications.host-app', 'close'),
    {provider: 'x'},
  ]);
  assert.deepEqual(apps.map(app => [app.id, app.title]), [
    ['app.host:chromium-browser', 'Chromium'],
    ['app.host:org.libreoffice.LibreOffice.writer', 'LibreOffice Writer'],
  ]);
  assert.deepEqual(apps[0].target, {provider: `workspace:${ROOM}`, address: {resource: 'app.host:chromium-browser', action: 'open'}});
});

test('the node session lists its legacy apps and opens the one the person picks', async () => {
  const timers = [];
  const listed = [];
  const invoked = [];
  const queries = [];
  let requests = [];
  const nodeApps = createNodeApps({
    room: ROOM,
    nodeSession: async () => 'ab'.repeat(32),
    actions: device => ({
      catalogue: async query => { queries.push({device, query}); return {rows: [row('app.host:chromium-browser', 'Chromium')], exhausted: true}; },
      invoke: async target => { invoked.push({device, target}); return null; },
    }),
    setApps: apps => listed.push(apps),
    takeRequests: () => { const taken = requests; requests = []; return taken; },
    setInterval: (callback, ms) => { timers.push({callback, ms}); return timers.length; },
    clearInterval: () => {},
  });
  await nodeApps.refresh();
  assert.deepEqual(listed.at(-1), [{id: 'app.host:chromium-browser', title: 'Chromium'}]);
  assert.equal(queries[0].device, 'ab'.repeat(32));
  assert.equal(queries[0].query.provider, `workspace:${ROOM}`);
  requests = ['app.host:chromium-browser', 'app.host:unknown'];
  timers.find(timer => timer.ms === 250).callback();
  assert.deepEqual(invoked, [{device: 'ab'.repeat(32), target: {provider: `workspace:${ROOM}`, address: {resource: 'app.host:chromium-browser', action: 'open'}}}]);
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
