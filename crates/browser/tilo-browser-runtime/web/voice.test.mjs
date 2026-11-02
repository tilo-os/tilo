import test from 'node:test';
import assert from 'node:assert/strict';

import {createVoiceCall, GRANT_DEADLINE_MS, GRANT_LOOK_MS, NO_GRANT, TURN_MS, base64OfDecoded} from './voice.mjs';
import {createPeerVoice} from './voice-peer.mjs';

const CALL = 'c'.repeat(64);
/** This page's own participation in the call's presence: its device. */
const ME = 'a'.repeat(64);
const BOB = 'b'.repeat(64);
/** The host's device: the one its bridge seals this page's frames as. */
const HOST = 'f'.repeat(64);
const drain = async () => { for (let index = 0; index < 20; index++) await Promise.resolve(); };

function grantFor({publish = true, others = [BOB]} = {}) {
  const participants = [ME, ...others].map(id => ({namespace: `${CALL}/${id}`, device: id, user: id, publishes: true}));
  return {
    call: CALL, epoch: 1, participation: `${CALL}/${ME}`,
    directions: {publish, subscribe: true},
    participants,
  };
}

/** A page whose host answers each turn with `answer`, and whose microphone,
 * playback and timers the test drives. */
function fixture() {
  const sent = [];
  const shows = [];
  const turns = [];
  let listener;
  let answer = {devices: [], audio: [], pictures: [], shows: [], keyframes: []};
  let failure;
  const signalling = {
    join: call => sent.push(['join', call]),
    leave: () => sent.push(['leave']),
    end: () => sent.push(['end']),
    reachable: () => sent.push(['reachable']),
    show: (...flags) => shows.push(flags),
    turn(json) {
      turns.push(JSON.parse(json));
      return failure ? Promise.reject(failure) : Promise.resolve(JSON.stringify(answer));
    },
    onEvent(next) { listener = next; return () => { listener = undefined; }; },
  };
  const audio = {started: 0, played: [], mutes: [], deafens: [], stopped: 0};
  Object.defineProperty(audio, 'captureActive', {get: () => !audio.muted && audio.stopped === 0});
  audio.start = async ({muted, onFrame, onLevels, onCaptureLost, onInputChanged, onError}) => {
    audio.started++;
    Object.assign(audio, {onFrame, onLevels, onCaptureLost, onInputChanged, onError, muted});
    return {
      play: frame => audio.played.push(frame),
      setDeafened: value => audio.deafens.push(value),
      setMuted: async value => {
        if (!value && audio.refuseCapture) throw Object.assign(new Error('Permission denied'), {name: 'NotAllowedError'});
        audio.mutes.push(value);
        audio.muted = value;
      },
      get captureActive() { return !audio.muted && audio.stopped === 0; },
      selectInput: async id => { audio.input = id; },
      selectOutput: async () => true,
      stop: async () => { audio.stopped++; },
    };
  };
  const timers = [];
  const clock = {
    now: () => clock.at,
    at: 1000,
    setTimeout: (callback, delay) => { timers.push({callback, delay}); return timers.length; },
    clearTimeout: handle => { if (timers[handle - 1]) timers[handle - 1].callback = () => {}; },
  };
  const environment = new EventTarget();
  const call = createVoiceCall({signalling, device: HOST, audio, environment, clock});
  const event = value => listener?.(value);
  return {sent, shows, turns, audio, timers, clock, environment, call, event, answer: value => { answer = value; }, fail: error => { failure = error; }};
}

/** Run the next turn the page scheduled, as its timer would. */
async function nextTurn(f) {
  const timer = f.timers.findLast(entry => entry.delay === TURN_MS && entry.callback);
  assert.ok(timer, 'a turn is scheduled');
  timer.callback();
  await drain();
}

/** Run the grant deadline's looks on a page that runs, one look at a time:
 * how many looks it took to end. */
function runDeadline({timers, clock}) {
  let looks = 0;
  for (let seen = 0; ; looks++) {
    const index = timers.findLastIndex(timer => timer.delay === GRANT_LOOK_MS);
    if (index < seen) return looks;
    seen = index + 1;
    clock.at += GRANT_LOOK_MS;
    timers[index].callback();
  }
}

const frame = n => ({timestamp: BigInt(n * 960), duration: 960, payload: Uint8Array.of(n)});

test('a joined call turns with the host: the microphone goes out sealed by the host device, and what it sends is played', async () => {
  const f = fixture();
  const states = [];
  f.call.subscribe(state => { if (states.at(-1) !== state.phase) states.push(state.phase); });
  f.call.join(CALL, {app: 'so.tilo.chat', record: ''});
  assert.deepEqual(f.sent, [['join', CALL]]);
  assert.deepEqual(f.shows.at(-1), [false, false, false, false], 'the call is joined muted and showing nothing');
  assert.equal(f.call.state.phase, 'joining');
  await drain();
  assert.equal(f.audio.started, 1, 'the audio starts within the gesture, before any grant');
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.turns.length, 1, 'the first turn follows the grant');
  assert.equal(f.turns[0].call, CALL);
  assert.equal(f.turns[0].app, 'so.tilo.chat');
  assert.equal(f.turns[0].record, '');
  assert.deepEqual(f.call.state.participants.map(participant => participant.self), [true, false]);

  f.audio.onFrame(frame(1));
  await nextTurn(f);
  assert.deepEqual(f.turns[1].audio, [{timestamp48k: 960, opus: btoa('\u0001')}]);
  assert.deepEqual(f.turns[1].audio.length && [...base64OfDecoded(f.turns[1].audio[0].opus)], [1]);

  f.answer({devices: [BOB], audio: [{from: BOB, timestamp48k: 960, opus: btoa('\u0002\u0003')}], pictures: [], shows: [], keyframes: []});
  await nextTurn(f);
  assert.equal(f.audio.played.length, 1);
  assert.equal(f.audio.played[0].publisherId, BOB);
  assert.equal(f.audio.played[0].timestamp, 960n, 'a frame names its place in its sender\'s 48 kHz timeline');
  assert.deepEqual([...f.audio.played[0].payload], [2, 3]);
  assert.equal(f.call.stats().turns.audio, 1);
  assert.deepEqual(states, ['joining', 'connecting', 'live']);
});

test('a datagram that repeats the frames before its newest one plays each frame once', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  const heard = sequence => ({from: BOB, sequence, timestamp48k: sequence * 960, opus: btoa(String.fromCharCode(sequence))});
  const played = () => f.audio.played.map(frame => [...frame.payload][0]);
  f.answer({devices: [BOB], audio: [heard(5), heard(6), heard(7)], pictures: [], shows: [], keyframes: []});
  await nextTurn(f);
  assert.deepEqual(played(), [5, 6, 7]);
  f.answer({devices: [BOB], audio: [heard(7), heard(8), heard(9)], pictures: [], shows: [], keyframes: []});
  await nextTurn(f);
  assert.deepEqual(played(), [5, 6, 7, 8, 9], 'the frames a datagram carries again play once');
  f.answer({devices: [BOB], audio: [heard(0), heard(1)], pictures: [], shows: [], keyframes: []});
  await nextTurn(f);
  assert.deepEqual(played(), [5, 6, 7, 8, 9, 0, 1], 'a sender whose stream started over plays');
});

test('muting stops what the microphone hands the turns; a call that may not publish mutes and says so', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setMuted(true);
  f.audio.onFrame(frame(1));
  await nextTurn(f);
  assert.deepEqual(f.turns.at(-1).audio, [], 'a muted microphone sends nothing');
  assert.equal(f.audio.captureActive, false);
  await f.call.setMuted(false);
  f.audio.onFrame(frame(2));
  await nextTurn(f);
  assert.equal(f.turns.at(-1).audio.length, 1);

  f.event({kind: 'granted', grant: grantFor({publish: false})});
  await drain();
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.canPublish, false);
});

test('a call the host refuses waits for its next grant, and one that never comes ends with a reason', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.fail(new Error('the host refused the turn: not admitted to this workspace'));
  f.audio.onFrame(frame(1));
  await nextTurn(f);
  assert.equal(f.call.state.phase, 'reconnecting');
  assert.deepEqual(f.sent.filter(([what]) => what === 'reachable').length, 1);
  f.fail(undefined);
  f.audio.onFrame(frame(2));
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(f.call.state.phase, 'live', 'the fresh grant takes turns again');
  assert.equal(f.turns.at(-1).audio.length, 1);

  f.fail(new Error('the host refused the turn: not admitted to this workspace'));
  await nextTurn(f);
  assert.equal(f.call.state.phase, 'reconnecting');
  runDeadline(f);
  assert.equal(f.call.state.phase, 'ended');
  assert.match(f.call.state.reason, /not admitted/);
  assert.equal(f.call.state.admitted, true, 'it was in a call it lost');
});

test('a host that cannot be reached retries with the microphone quiet, and the call goes on when it answers', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.fail(new Error('unreachable'));
  f.audio.onFrame(frame(1));
  await nextTurn(f);
  assert.equal(f.call.state.phase, 'reconnecting');
  assert.equal(f.audio.captureActive, false, 'nothing is captured behind a link that is not there');
  assert.equal(f.call.stats().turns.unreachable, 1);
  f.fail(undefined);
  // The retry's timer opens the next turn.
  const retry = f.timers.findLast(entry => entry.callback && entry.delay === 250);
  assert.ok(retry, 'a retry is scheduled');
  retry.callback();
  await nextTurn(f);
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.audio.captureActive, true, 'the microphone comes back with the host');
});

test('a call this page holds no keys for ends at once with the host\'s reason', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.fail(new Error('this page holds no keys for the call\'s record'));
  await nextTurn(f);
  assert.equal(f.call.state.phase, 'ended');
  assert.match(f.call.state.reason, /no keys/);
  assert.ok(f.sent.some(([what]) => what === 'end'), 'the host\'s bridge forgets the page');
});

test('speaking follows the measured levels, held briefly', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  const starts = [];
  f.call.subscribe(state => starts.push(state.participants.map(participant => participant.self)));
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.audio.onLevels({capture: 0, publishers: new Map([[BOB, 0.5]])});
  assert.equal(f.call.state.participants.find(participant => participant.device === BOB).speaking, true);
  f.clock.at += 100;
  f.audio.onLevels({capture: 0.5, publishers: new Map()});
  assert.equal(f.call.state.participants.find(participant => participant.self).speaking, true, 'this page speaks as the host\'s device');
  f.clock.at += 500;
  f.audio.onLevels({capture: 0, publishers: new Map()});
  assert.equal(f.call.state.participants.find(participant => participant.device === BOB).speaking, false, 'a held level ends');
});

test('leaving ends the page\'s call on the host too; an ended call stops everything', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.call.leave();
  assert.deepEqual(f.sent.slice(-2), [['end'], ['leave']]);
  assert.equal(f.call.state.phase, 'idle');
  assert.equal(f.audio.stopped, 1);
  assert.deepEqual(f.sent.filter(([what]) => what === 'end').length, 1);
  const count = f.turns.length;
  for (const timer of f.timers) timer.callback();
  await drain();
  assert.equal(f.turns.length, count, 'no turn outlives the call');
});

test('a join the host never grants ends at the deadline; a grant in time keeps it', async () => {
  const never = fixture();
  never.call.join(CALL);
  await drain();
  runDeadline(never);
  assert.equal(never.call.state.phase, 'ended');
  assert.equal(never.call.state.reason, NO_GRANT);
  assert.equal(never.call.state.admitted, false);

  const inTime = fixture();
  inTime.call.join(CALL);
  await drain();
  inTime.clock.at += GRANT_DEADLINE_MS / 2;
  inTime.event({kind: 'granted', grant: grantFor()});
  await drain();
  runDeadline(inTime);
  assert.equal(inTime.call.state.phase, 'live');
});

test('deafening silences the call and mutes; undeafening puts the microphone back as it was', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setDeafened(true);
  assert.equal(f.call.state.deafened, true);
  assert.equal(f.call.state.muted, true);
  assert.equal(f.audio.deafens.at(-1), true);
  assert.deepEqual(f.shows.at(-1), [false, false, true, true], 'everyone sees who hears nothing');
  await f.call.setDeafened(false, {unmute: true});
  assert.equal(f.call.state.deafened, false);
  assert.equal(f.call.state.muted, false);
  assert.equal(f.audio.deafens.at(-1), false);
});

test('a refused unmute keeps the person in the call, muted, until they allow it', async () => {
  const f = fixture();
  f.call.join(CALL, {muted: true});
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.audio.refuseCapture = true;
  await f.call.setMuted(false);
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.micIssue, 'permission_denied');
  f.audio.refuseCapture = false;
  await f.call.setMuted(false);
  assert.equal(f.call.state.micIssue, null);
  assert.equal(f.call.state.muted, false);
});

test('a participant plays at the volume set for them', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.call.setVolume(BOB, 0.25);
  f.call.setVolume('not a person', 2);
  f.answer({devices: [BOB], audio: [{from: BOB, timestamp48k: 0, opus: btoa('\u0001')}], pictures: [], shows: [], keyframes: []});
  await nextTurn(f);
  assert.equal(f.audio.played[0].gain, 0.25);
  assert.equal(f.call.state.phase, 'live');
});

test('a microphone that fails at start ends the participation instead of claiming a live call', async () => {
  const f = fixture();
  f.audio.start = async () => { throw new Error('no output device'); };
  f.call.join(CALL);
  await drain();
  assert.equal(f.call.state.phase, 'ended');
  assert.equal(f.call.state.audioFailed, true);
  assert.ok(f.sent.some(([what]) => what === 'end'));
});

test('what the app shows of the call\'s devices is what the page asks them for, and a keyframe until one comes', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.call.showPictures([{person: BOB, source: 'camera', layer: 'mid'}]);
  await nextTurn(f);
  assert.deepEqual(f.turns.at(-1).shows, [{device: BOB, camera: [1], screen: []}]);
  assert.deepEqual(f.turns.at(-1).keyframes, [{device: BOB, source: 0}], 'nothing before a keyframe decodes');
  f.answer({devices: [BOB], audio: [], pictures: [
    {from: BOB, source: 0, keyframe: true, object: btoa('¥\u0000')},
  ], shows: [], keyframes: []});
  await nextTurn(f);
  const taken = f.call.takePictures(0);
  assert.equal(taken.pictures.length, 1);
  assert.equal(taken.pictures[0].person, BOB);
  assert.equal(taken.pictures[0].source, 'camera');
  await nextTurn(f);
  assert.deepEqual(f.turns.at(-1).keyframes, [], 'the keyframe arrived');
});

test('peer.voice takes the page\'s turns through the wasm peer and lists its calls', async () => {
  const wasm = {
    joined: [],
    turns: [],
    ended: [],
    voiceJoin: (workspace, call) => wasm.joined.push([workspace, call]),
    voiceLeave: () => {},
    voiceCalls: () => {},
    voiceShow: (workspace, camera, screen, muted, deafened) => wasm.shown = [camera, screen, muted, deafened],
    callLaneTurn(workspace, json) { wasm.turns.push([workspace, JSON.parse(json)]); return Promise.resolve(JSON.stringify({devices: [], audio: [], pictures: [], shows: [], keyframes: []})); },
    callLaneLeave: workspace => wasm.ended.push(workspace),
  };
  const voice = createPeerVoice({peer: () => wasm, workspace: 'the-workspace', device: HOST, audio: {
    start: async () => ({play() {}, setMuted: async () => {}, stop: async () => {}, get captureActive() { return false; }}),
  }});
  voice.call.join(CALL, {app: 'so.tilo.chat', record: ''});
  await drain();
  assert.deepEqual(wasm.joined, [['the-workspace', CALL]]);
  assert.deepEqual(wasm.shown, [false, false, false, false]);
  // The host's grant comes through the wasm peer's events and its turn
  // through callLaneTurn, then its bridge is told the page left.
  voice.pump(JSON.stringify([{room: 'the-workspace', kind: 'granted', grant: grantFor()}]));
  await drain();
  assert.equal(wasm.turns.length, 1);
  assert.equal(wasm.turns[0][0], 'the-workspace');
  assert.equal(wasm.turns[0][1].call, CALL);
  assert.equal(voice.call.state.phase, 'live');
  voice.call.leave();
  assert.deepEqual(wasm.ended, ['the-workspace']);
  assert.equal(voice.call.state.phase, 'idle');
  voice.dispose();
});
