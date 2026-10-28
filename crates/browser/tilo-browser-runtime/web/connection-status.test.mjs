import test from 'node:test';
import assert from 'node:assert/strict';

import {connectionStatus, followConnection} from './connection-status.mjs';

/** A peer's `PeerSnapshot` as the session reports it. */
function snapshot({phase = 'ready', transport = 'connected', admission = 'ready', failures = [], outgoingPending = 0} = {}) {
  return {phase, errorCode: null, outgoingPending, synchronization: outgoingPending ? 'pending' : 'acknowledged',
    health: {version: 1, transport: {phase: transport}, admission: {phase: admission}, records: [], failures}};
}

const drawn = {phase: 'presented'};
const well = {peer: snapshot(), desktop: drawn, mounted: true, saved: true};

test('a desktop that drew on a healthy session says nothing, and keeps saying it', () => {
  assert.deepEqual(connectionStatus(well), {state: 'online', sentence: null, detail: null});
});

test('a session that keeps committing online is still online', () => {
  assert.equal(connectionStatus({...well, peer: snapshot({phase: 'admitting', admission: 'syncing', outgoingPending: 2})}).state,
    'connecting', 'commits on their way are not an outage');
});

test('a transport that dropped after the mount says off line, not connected', () => {
  for (const transport of ['offline', 'reconnecting', 'failed', 'closed']) {
    const reading = connectionStatus({...well, peer: snapshot({phase: 'incomplete', transport})});
    assert.equal(reading.state, 'offline', transport);
    assert.match(reading.sentence, /offline/);
    assert.match(reading.sentence, /kept on this device/, 'the person is told their work stays');
    assert.match(reading.detail, new RegExp(transport));
  }
});

test('a page the host stopped admitting says so, and its ending session says it is over', () => {
  assert.equal(connectionStatus({...well, peer: snapshot({phase: 'failed', transport: 'connected', admission: 'failed'})}).state, 'offline');
  const refused = connectionStatus({...well, peer: snapshot({phase: 'incomplete', admission: 'failed'})});
  assert.equal(refused.state, 'admission');
  assert.match(refused.sentence, /kept on this device/);
});

test('changes the store could not commit are said, from the store and from the session alike', () => {
  assert.equal(connectionStatus({...well, saved: false}).state, 'saving');
  const reported = connectionStatus({...well, peer: snapshot({failures: [{scope: 'storage', code: 'storage-transaction-failed'}]})});
  assert.equal(reported.state, 'saving');
  assert.match(reported.sentence, /aren’t saved on this device yet/);
});

test('a desktop that stopped drawing is said in the person’s words, with its cause kept for the console', () => {
  const reading = connectionStatus({...well, desktop: {phase: 'failed', error: 'device lost'}});
  assert.equal(reading.state, 'renderer');
  assert.equal(reading.sentence, 'The desktop stopped drawing. Try again to start it, or reload the page.');
  assert.match(reading.detail, /device lost/);
});

test('nothing is said before the desktop drew, or before a peer session is ready', () => {
  assert.equal(connectionStatus({...well, desktop: null}).state, 'connecting');
  assert.equal(connectionStatus({...well, mounted: false}).state, 'connecting');
  assert.equal(connectionStatus({...well, peer: snapshot({phase: 'authenticating', transport: 'connecting', admission: 'waiting'})}).state, 'connecting');
  assert.equal(connectionStatus({...well, peer: null}).state, 'online', 'a page of the person’s own has no session to lose');
});

test('the bar says the worst thing first and clears once it is over', () => {
  const shown = [];
  let peer = snapshot();
  let desktop = drawn;
  const follow = followConnection({read: () => ({peer, desktop, mounted: true, saved: true}), bar: {show: reading => shown.push(reading)}});
  assert.deepEqual(shown, [], 'a healthy desktop says nothing');
  peer = snapshot({phase: 'incomplete', transport: 'offline'});
  follow.look();
  assert.deepEqual(shown.map(reading => reading.state), ['offline']);
  // The transport came back while the store still could not save: the worse
  // line stays until the desktop is well again.
  peer = snapshot();
  desktop = {phase: 'failed', error: 'device lost'};
  follow.look();
  assert.deepEqual(shown.map(reading => reading.state), ['offline'], 'a milder trouble waits for the worse one to clear');
  desktop = drawn;
  follow.look();
  assert.deepEqual(shown.map(reading => reading.state), ['offline', 'online']);
  assert.equal(shown[1].cleared, true, 'the bar clears rather than saying connected');
  assert.equal(shown[1].sentence, null);
});

test('the cause of each line goes to the console once, not to the bar', () => {
  const debug = [];
  const peer = snapshot({phase: 'incomplete', transport: 'reconnecting'});
  const follow = followConnection({read: () => ({peer, desktop: drawn, mounted: true, saved: true}),
    bar: {showDebug: (state, detail) => debug.push([state, detail])}});
  follow.look();
  follow.look();
  assert.deepEqual(debug, [['offline', 'session incomplete, transport reconnecting']]);
});

test('a peer read as it is disposed says nothing rather than keeping a line', () => {
  const follow = followConnection({read: () => { throw new Error('Shared resource connection closed'); }});
  assert.equal(follow.look(), undefined);
});
