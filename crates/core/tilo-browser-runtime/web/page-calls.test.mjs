import assert from 'node:assert/strict';
import {test} from 'node:test';

import {createPageCalls, WATCH_HOLD_MS} from './page-calls.mjs';

const ADA = 'a'.repeat(63) + '1';
const BO = 'b'.repeat(63) + '2';

/** A page's voice as the browser's call gives it, with what it was asked to do. */
function voiceOf({members = []} = {}) {
  const listeners = new Set();
  const callListeners = new Set();
  const asked = [];
  const state = {phase: 'idle', call: null, muted: false, camera: false, screen: false, deafened: false, inputId: null};
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
