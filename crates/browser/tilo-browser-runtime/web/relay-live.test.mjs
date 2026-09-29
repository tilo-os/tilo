import test from 'node:test';
import assert from 'node:assert/strict';
import {createRelayLive, relayLiveCarriers} from './relay-live.mjs';

const ROOM = 'aa'.repeat(32);
const ME = 'bb'.repeat(32);
const THEM = 'cc'.repeat(32);
const WRITER = {app: 'so.tilo.writer', record: 'main'};

// JSON stands in for the canonical frames: the relay layer only routes them.
const codec = {
  encodeLivePresence: json => new TextEncoder().encode(JSON.stringify({kind: 'presence', frame: JSON.parse(json)})),
  encodeLivePreview: (record, seq, ops) => new TextEncoder().encode(JSON.stringify({kind: 'preview', record: JSON.parse(record), seq: Number(seq), ops: JSON.parse(ops)})),
  encodeLivePreviewEnd: record => new TextEncoder().encode(JSON.stringify({kind: 'previewEnd', record: JSON.parse(record)})),
  decodeLiveFrame: bytes => new TextDecoder().decode(bytes),
  liveRecordTrack: record => `ops/${JSON.parse(record).record}`,
};

function fakePeer() {
  const durable = [];
  return {
    durable,
    liveGrant: async room => JSON.stringify({token: '0102', room: ROOM, publisher: ME, expiresAtNs: (Date.now() + 3_600_000) * 1e6, asked: room}),
    takeDurableEvents: () => JSON.stringify(durable.splice(0)),
  };
}

function fakeCarriers() {
  const sent = {presence: [], ops: []};
  const handlers = {};
  let connected;
  const session = {
    closed: new Promise(() => {}),
    publishPresence: bytes => { sent.presence.push(JSON.parse(new TextDecoder().decode(bytes))); return true; },
    publishOps: (track, bytes) => { sent.ops.push({track, frame: JSON.parse(new TextDecoder().decode(bytes))}); return true; },
    closeOps() {},
    watchMembers: ({onMember}) => { handlers.member = onMember; },
    subscribePresence: (from, onFrame) => { handlers[`presence:${from}`] = onFrame; return () => {}; },
    subscribeOps: (from, track, onFrame) => { handlers[`${track}:${from}`] = onFrame; return () => {}; },
    close() {},
  };
  return {
    sent, handlers,
    connect: async options => { connected = options; return session; },
    get options() { return connected; },
  };
}

const settle = () => new Promise(resolve => setTimeout(resolve, 20));
const frame = value => new TextEncoder().encode(JSON.stringify(value));

test('a relay room goes live with its grant and publishes presence with the participant', async () => {
  const carriers = fakeCarriers();
  const live = createRelayLive(fakePeer(), codec, 'room-uuid', {
    endpoint: 'https://room.test:9700/moq/live', carriers, participant: {label: 'Phone', color: '#30a46c'},
  });
  await settle();
  assert.equal(live.api.state, 'live');
  assert.equal(live.api.publisherId, ME);
  assert.deepEqual(carriers.options.namespace, ['tilo-live', ROOM, ME]);
  assert.deepEqual([...carriers.options.authorization], [1, 2]);
  live.api.publishPresence({focus: WRITER, selection: {anchor: '01', head: '02'}});
  await settle();
  const last = carriers.sent.presence.at(-1).frame;
  assert.deepEqual(last.selection, {anchor: '01', head: '02'});
  assert.deepEqual(last.participant, {kind: 'person', label: 'Phone', color: '#30a46c', activity: null});
  live.stop();
  assert.equal(live.api.available, false);
});

test('a member\'s caret reaches remoteState and its label reaches the overlay\'s members', async () => {
  const carriers = fakeCarriers();
  const live = createRelayLive(fakePeer(), codec, 'room-uuid', {endpoint: 'https://room.test:9700/moq/live', carriers});
  await settle();
  const members = [];
  live.api.onMember(value => members.push(value));
  carriers.handlers.member(THEM, true);
  assert.deepEqual(members, [{publisher: THEM, active: true}]);
  assert.equal(live.api.memberCount, 1);
  carriers.handlers[`presence:${THEM}`](frame({kind: 'presence', frame: {
    seq: 1, sentAtMs: Date.now(), focus: WRITER, pointer: null, selection: {anchor: 'aa', head: 'aa'}, drag: null,
    participant: {kind: 'person', label: 'Visitor', color: '#e5484d', activity: null},
  }}), performance.now());
  assert.deepEqual(members.at(-1), {publisher: THEM, active: true}, 'a name that arrives is announced');
  assert.equal(members.length, 2);
  const [remote] = live.api.remoteState();
  assert.equal(remote.publisher, THEM);
  assert.deepEqual(remote.selection, {anchor: 'aa', head: 'aa'});
  assert.deepEqual(live.members(), [{device: THEM, name: 'Visitor', color: '#e5484d'}]);
  // Focusing a record subscribes the member's typing previews on it.
  const previews = [];
  live.api.onPreview(value => previews.push(value));
  carriers.handlers[`ops/main:${THEM}`](frame({kind: 'preview', record: WRITER, seq: 1, ops: [{kind: 'insert', at: 'aa', text: 'hi'}]}));
  assert.equal(previews[0].ops[0].text, 'hi');
  carriers.handlers.member(THEM, false);
  assert.deepEqual(live.api.remoteState(), []);
  live.stop();
});

test('durable events from the peer drop previews and reach onDurable', async () => {
  const peer = fakePeer();
  const live = createRelayLive(peer, codec, 'room-uuid', {endpoint: 'https://room.test:9700/moq/live', carriers: fakeCarriers()});
  await settle();
  const landed = [];
  live.api.onDurable(value => landed.push(value.record));
  peer.durable.push({kind: 'durable', record: WRITER});
  live.pump();
  assert.deepEqual(landed, [WRITER]);
  live.stop();
});

test('a loopback room tries WebTransport on the TLS port and falls back to its plain origin', async () => {
  const asked = [];
  const fetch = async url => { asked.push(String(url)); throw Object.assign(new Error('offline'), {code: 'unavailable'}); };
  const tried = [];
  const webtransport = {connect: async options => { tried.push(options.endpoint); throw new Error('no UDP'); }};
  const {endpoint, carriers} = relayLiveCarriers('http://localhost:9764', {webtransportPort: 9700, certificateSha256: null},
    {fetch, webtransport, hasWebTransport: () => true});
  assert.equal(endpoint, 'https://localhost:9700/moq/live');
  await assert.rejects(carriers.connect({
    endpoint, authorization: new Uint8Array([1]), namespace: ['tilo-live', ROOM, ME], directions: {publish: true, subscribe: true},
  }));
  assert.deepEqual(tried, ['https://localhost:9700/moq/live']);
  assert.ok(asked.length > 0 && asked.every(url => url.startsWith('http://localhost:9764/')), asked.join(' '));
});
