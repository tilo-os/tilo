import assert from 'node:assert/strict';
import {test} from 'node:test';

import {createPageCalls, WATCH_HOLD_MS} from './page-calls.mjs';

const ADA = 'a'.repeat(63) + '1';
const BO = 'b'.repeat(63) + '2';

/** A page's voice as the browser's call gives it, with what it was asked to do. */
function voiceOf({members = []} = {}) {
  const listeners = new Set();
  const callListeners = new Set();
  const pictureListeners = new Set();
  const asked = [];
  const state = {phase: 'idle', call: null, muted: false, camera: false, screen: false, deafened: false, inputId: null, members: members.map(member => ({user: member.user ?? member, self: member.self === true}))};
  const peerVoice = {
    call: {
      get state() { return state; },
      join(channel, {muted = false} = {}) {
        asked.push(['join', channel, muted]);
        state.phase = 'joining'; state.call = channel; state.muted = muted; emit();
      },
      leave() { asked.push(['leave']); state.phase = 'idle'; state.call = null; emit(); },
      setMuted(muted) { asked.push(['muted', muted]); state.muted = muted; emit(); },
      setDeafened(deafened) { asked.push(['deafened', deafened]); state.deafened = deafened; emit(); },
      setVideo(source, on) { asked.push([source, on]); state[source] = on; emit(); },
      selectInput(id) { asked.push(['input', id]); state.inputId = id; emit(); },
      setVolume(member, gain) { asked.push(['volume', member, gain]); },
      showPictures(items) { asked.push(['show', items]); },
      onPicture(listener) { pictureListeners.add(listener); return () => pictureListeners.delete(listener); },
      subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    },
    calls: [],
    onCalls(listener) { callListeners.add(listener); return () => callListeners.delete(listener); },
  };
  function emit() { for (const listener of [...listeners]) listener(state); }
  return {
    peerVoice,
    asked,
    state,
    /** What the workspace's presence says now, as the next listing. */
    lists(calls) { peerVoice.calls = calls; for (const listener of [...callListeners]) listener(calls); },
    /** One picture this page's call decoded: who it is of, and the frame. */
    picture(person, source, frame) {
      for (const listener of [...pictureListeners]) listener(person, source, frame);
    },
    members,
  };
}

const step = (requestId, op, fields = {}) => ({
  request_id: requestId,
  command: {version: 1, intent: requestId, command: {op, ...fields}},
});

test('a join is carried by the browser and answered with what the call now is', () => {
  const {peerVoice, asked, lists} = voiceOf();
  lists([{channel: 'standup', people: [ADA], cameras: [], screens: [], muted: [ADA], deafened: [], bridges: [], bridged: []}]);
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});

  calls.apply(step(7, 'join', {channel_id: 'standup'}));

  assert.deepEqual(asked[0], ['join', 'standup', true]);
  assert.equal(sent.length, 1, 'the step is answered at once');
  assert.equal(sent[0].request_id, 7);
  const status = sent[0].answer.answered;
  assert.equal(status.state, 'joining');
  assert.deepEqual(status.rosters.map(roster => roster.channel_id), ['standup']);
  assert.deepEqual(status.rosters[0].people, [ADA]);
  assert.deepEqual(status.rosters[0].muted, [ADA]);
  assert.equal(status.muted, true, 'a call starts muted');
  assert.ok(Number.isSafeInteger(status.revision), 'the status carries a revision');
});

test('the switches and the camera and screen steps are the browser call input', () => {
  const {peerVoice, asked} = voiceOf();
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  calls.apply(step(1, 'join', {channel_id: 'standup'}));
  calls.apply(step(2, 'set_muted', {muted: false}));
  calls.apply(step(3, 'set_deafened', {deafened: true}));
  calls.apply(step(4, 'set_camera', {on: true}));
  calls.apply(step(5, 'set_screen', {on: true}));

  assert.deepEqual(asked.slice(1, 5), [
    ['muted', false],
    ['deafened', true],
    ['camera', true],
    ['screen', true],
  ]);
  const last = sent.at(-1).answer.answered;
  assert.equal(last.camera, true);
  assert.equal(last.screen, true);
  assert.equal(last.deafened, true);
});

test('the status offers the browser\'s microphones and cameras, and a camera chosen is sent from', async () => {
  const {peerVoice, asked} = voiceOf();
  peerVoice.call.inputs = async () => [{deviceId: 'mic-1', label: 'Headset'}, {deviceId: '', label: ''}];
  peerVoice.call.cameras = async () => [{deviceId: 'cam-1', label: 'Built-in'}, {deviceId: 'cam-2', label: 'Desk camera'}];
  peerVoice.call.selectCamera = id => { asked.push(['camera-from', id]); peerVoice.call.state.cameraId = id; };
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  // A watch is answered once the browser has listed what it has.
  calls.apply(step(1, 'watch', {revision: calls.status().revision}));
  await new Promise(resolve => setImmediate(resolve));

  const listed = sent.at(-1).answer.answered;
  assert.deepEqual(listed.inputs, [{id: 'mic-1', label: 'Headset'}], 'a device without an id is not offered');
  assert.deepEqual(listed.cameras, [{id: 'cam-1', label: 'Built-in'}, {id: 'cam-2', label: 'Desk camera'}]);
  assert.equal(listed.camera_id, 'cam-1', 'until the person chooses, the browser sends from its first camera');

  calls.apply(step(2, 'select_camera', {camera_id: 'cam-2'}));
  assert.deepEqual(asked.at(-1), ['camera-from', 'cam-2']);
  assert.equal(sent.at(-1).answer.answered.camera_id, 'cam-2');
});

test('a watch is held until who is in the call moves, then answered with the roster', () => {
  const {peerVoice, lists} = voiceOf();
  lists([{channel: 'standup', people: [ADA], cameras: [], screens: [], muted: [], deafened: [], bridges: [], bridged: []}]);
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  const first = calls.status();

  calls.apply(step(9, 'watch', {revision: first.revision}));
  assert.equal(sent.length, 0, 'nothing moved, so the watch holds');

  lists([{channel: 'standup', people: [ADA, BO], cameras: [], screens: [], muted: [], deafened: [], bridges: [], bridged: []}]);
  assert.equal(sent.length, 1, 'who joined answers the watch at once');
  assert.equal(sent[0].request_id, 9);
  assert.deepEqual(sent[0].answer.answered.rosters[0].people, [ADA, BO]);
});

test('a watch nothing moves is answered when its hold ends', () => {
  const {peerVoice, lists} = voiceOf();
  lists([{channel: 'standup', people: [ADA], cameras: [], screens: [], muted: [], deafened: [], bridges: [], bridged: []}]);
  const timers = [];
  const sent = [];
  const calls = createPageCalls({
    peerVoice,
    send: answer => sent.push(answer),
    setTimeout: (callback, ms) => { timers.push({callback, ms}); return timers.length; },
    clearTimeout: () => {},
  });
  calls.apply(step(4, 'watch', {revision: calls.status().revision}));
  assert.equal(timers.length, 1);
  assert.equal(timers[0].ms, WATCH_HOLD_MS);
  assert.equal(sent.length, 0);

  timers[0].callback();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].request_id, 4);
});

test('a status asked for with no call is idle and answers no roster', () => {
  const {peerVoice} = voiceOf();
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  calls.apply(step(1, 'status'));
  assert.equal(sent[0].answer.answered.state, 'idle');
  assert.deepEqual(sent[0].answer.answered.rosters, []);
});

test('a step the page has no way to carry is refused, in the browser’s words', () => {
  const {peerVoice} = voiceOf();
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  calls.apply(step(3, 'levitate'));
  assert.equal(sent[0].request_id, 3);
  assert.match(sent[0].answer.refused, /levitate/);
});

test("the call’s pictures are decoded where the tiles stand, and the view is told so", () => {
  const {peerVoice, picture} = voiceOf({members: [{user: ADA, self: true}, BO]});
  const sent = [];
  const drawn = [];
  const calls = createPageCalls({
    peerVoice,
    send: answer => sent.push(answer),
    paint: (person, source, frame) => drawn.push([person, source, frame]),
  });
  calls.apply(step(1, 'join', {channel_id: 'standup'}));
  assert.equal(sent[0].answer.answered.pictures_local, true, 'the view asks for no collection');
  assert.equal(sent[0].answer.answered.pictures, undefined, 'and none is carried');

  const frame = {displayWidth: 640, displayHeight: 360};
  picture(BO, 'camera', frame);
  assert.deepEqual(drawn, [[BO, 'camera', frame]], 'the frame is drawn where its tile stands');
});

test('the tiles hear the call move as it happens, and only when it moves', () => {
  const {peerVoice, lists} = voiceOf({members: [{user: ADA, self: true}]});
  lists([{channel: 'standup', people: [ADA], cameras: [], screens: [], muted: [ADA], deafened: [], bridges: [], bridged: []}]);
  const heard = [];
  const calls = createPageCalls({peerVoice, send: () => {}, onStatus: (status, me) => heard.push([status.speaking, status.muted, me])});
  assert.equal(heard.length, 1, 'the status is told as the page opens');
  assert.deepEqual(heard[0], [[], false, ADA], 'this page is its person');

  calls.status();
  assert.equal(heard.length, 1, 'the same status is not told twice');

  lists([{channel: 'standup', people: [ADA, BO], cameras: [], screens: [], muted: [], deafened: [], bridges: [], bridged: []}]);
  assert.equal(heard.length, 2, 'a person who joins moves the tiles');
});

test('a pictures step is no longer carried: the page draws them where they stand', () => {
  const {peerVoice} = voiceOf();
  const sent = [];
  const calls = createPageCalls({peerVoice, send: answer => sent.push(answer)});
  calls.apply(step(5, 'pictures', {after: 0}));
  assert.match(sent[0].answer.refused, /pictures/);
});
