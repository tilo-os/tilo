import test from 'node:test';
import assert from 'node:assert/strict';

import {createVoiceCall, GRANT_DEADLINE_MS, GRANT_LOOK_MS, NO_GRANT} from './voice.mjs';
import {createPeerVoice} from './voice-peer.mjs';

const CALL = 'c'.repeat(64);
/** This page's person. */
const ME = 'a'.repeat(64);
const BOB = 'b'.repeat(64);
/** The host's device: its participation in the call, which seals this
 * page's frames as their sender. */
const HOST = 'f'.repeat(64);
const drain = async () => { for (let index = 0; index < 20; index++) await Promise.resolve(); };

function grantFor({publish = true, others = [BOB]} = {}) {
  const participants = [{device: HOST, user: ME}, ...others.map(id => ({device: id, user: id}))]
    .map(participant => ({...participant, publishes: true}));
  return {
    call: CALL, epoch: 1,
    directions: {publish, subscribe: true},
    participants,
  };
}

/** A page whose microphone, playback, camera encoders and timers the test
 * drives, and which records what its signalling sends. */
function fixture() {
  const sent = [];
  const shows = [];
  const pictures = [];
  const showing = [];
  const keyframes = [];
  let listener;
  const published = [];
  const signalling = {
    join: call => sent.push(['join', call]),
    leave: () => sent.push(['leave']),
    show: (...flags) => shows.push(flags),
    audio: frames => published.push(...frames),
    picture: batch => pictures.push(...batch),
    showing: list => showing.push(list),
    keyframe: (device, source) => keyframes.push([device, source]),
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
  /** The encoders the page made, by source. */
  const encoders = new Map();
  const encoder = ({source, wanted, onPicture, onEnded}) => {
    const made = {source, wanted, onPicture, onEnded, keyframes: 0, stopped: 0, stop: () => { made.stopped++; }, keyframe: () => { made.keyframes++; }};
    encoders.set(source, made);
    return made;
  };
  const timers = [];
  const clock = {
    now: () => clock.at,
    at: 1000,
    setTimeout: (callback, delay) => { timers.push({callback, delay}); return timers.length; },
    clearTimeout: handle => { if (timers[handle - 1]) timers[handle - 1].callback = () => {}; },
  };
  const environment = new EventTarget();
  const track = {stopped: 0, stop() { this.stopped++; }};
  const open = async () => ({getTracks: () => [track]});
  const call = createVoiceCall({signalling, device: HOST, audio, environment, clock, encoder, open});
  const event = value => listener?.(value);
  return {sent, shows, pictures, showing, keyframes, published, audio, encoders, track, timers, clock, environment, call, event};
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

test('a joined call publishes its microphone on the plane and plays what the call publishes there', async () => {
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
  assert.deepEqual(f.call.state.participants.map(participant => participant.self), [true, false]);

  f.audio.onFrame(frame(1));
  assert.deepEqual(f.published, [{timestamp48k: 960, opus: '01'}], 'a frame is published as it is made');

  f.event({kind: 'heard', from: BOB, sequence: 3, timestamp48k: 960, opus: '0203'});
  assert.equal(f.audio.played.length, 1);
  assert.equal(f.audio.played[0].publisherId, BOB);
  assert.equal(f.audio.played[0].timestamp, 960n, 'a frame names its place in its sender\'s 48 kHz timeline');
  assert.deepEqual([...f.audio.played[0].payload], [2, 3]);
  assert.equal(f.call.stats().counters.heard, 1);
  assert.deepEqual(states, ['joining', 'live']);
});

test('a frame a plane announces again plays once, and a stream that started over plays', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  const heard = sequence => ({kind: 'heard', from: BOB, sequence, timestamp48k: sequence * 960, opus: sequence.toString(16).padStart(2, '0')});
  const played = () => f.audio.played.map(frame => [...frame.payload][0]);
  for (const sequence of [5, 6, 7]) f.event(heard(sequence));
  assert.deepEqual(played(), [5, 6, 7]);
  f.event(heard(7));
  assert.deepEqual(played(), [5, 6, 7], 'a plane joined again announces its stream\'s current object once more');
  f.event(heard(0));
  f.event(heard(1));
  assert.deepEqual(played(), [5, 6, 7, 0, 1], 'a sender whose stream started over plays');
});

test('a deafened page hears nothing of what the plane publishes', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setDeafened(true);
  f.event({kind: 'heard', from: BOB, sequence: 1, timestamp48k: 960, opus: '01'});
  assert.equal(f.audio.played.length, 0);
});

test('muting stops the microphone; a call that may not publish mutes and says so', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setMuted(true);
  f.audio.onFrame(frame(1));
  assert.deepEqual(f.published, [], 'a muted microphone publishes nothing');
  assert.equal(f.audio.captureActive, false);
  await f.call.setMuted(false);
  f.audio.onFrame(frame(2));
  assert.deepEqual(f.published, [{timestamp48k: 1920, opus: '02'}]);

  f.event({kind: 'granted', grant: grantFor({publish: false})});
  await drain();
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.canPublish, false);
});

test('a picture the plane delivers is admitted as it comes, with nothing polling for it', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.event({kind: 'picture', from: BOB, source: 'camera', keyframe: true, object: '0500'});
  const taken = f.call.takePictures(0);
  assert.equal(taken.pictures.length, 1);
  assert.equal(taken.pictures[0].person, BOB);
  assert.equal(taken.pictures[0].source, 'camera');
  assert.equal(taken.pictures[0].object, 'BQA=', 'the object as the app decodes it');
  assert.equal(f.call.stats().counters.pictures, 1);
  const again = f.call.takePictures(taken.cursor);
  assert.equal(again.pictures.length, 0, 'each picture is taken once');
});

test('what the app shows is told to the plane, and a device starting to show a source is asked for a keyframe', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.call.showPictures([{person: BOB, source: 'camera', layer: 'mid'}]);
  assert.deepEqual(f.showing.at(-1), [{device: BOB, camera: ['mid'], screen: []}], 'the plane reads exactly those layers');
  assert.deepEqual(f.keyframes, [[BOB, 'camera']], 'nothing before a keyframe decodes');
  f.event({kind: 'picture', from: BOB, source: 'camera', keyframe: true, object: '0500'});
  f.call.showPictures([{person: BOB, source: 'camera', layer: 'mid'}]);
  assert.deepEqual(f.keyframes, [[BOB, 'camera']], 'the keyframe arrived');
  f.call.showPictures([]);
  assert.deepEqual(f.showing.at(-1), [], 'a device the app no longer draws is unread');
});

test('the plane\'s demand says which layers to encode, and which sources start over', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setVideo('camera', true);
  const camera = f.encoders.get('camera');
  assert.ok(camera, 'showing the camera makes its encoder');
  assert.equal(camera.wanted('mid'), false, 'nothing is encoded before anyone shows it');
  f.event({kind: 'demand', wanted: [{source: 'camera', layers: ['mid']}], keyframes: ['camera']});
  assert.equal(camera.wanted('mid'), true);
  assert.equal(camera.wanted('high'), false);
  assert.equal(camera.keyframes, 1, 'a new reader starts at a keyframe');
  f.event({kind: 'demand', wanted: [{source: 'camera', layers: ['mid', 'low']}], keyframes: []});
  assert.equal(camera.wanted('low'), true);
  assert.equal(camera.keyframes, 1);
});

test('a picture an encoder makes is published as it is made', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  await f.call.setVideo('camera', true);
  const camera = f.encoders.get('camera');
  camera.onPicture({source: 'camera', layer: 'mid', sequence: 3, keyframe: true, timestampUs: 1000, width: 1280, height: 720, payload: Uint8Array.of(1, 2)});
  assert.deepEqual(f.pictures, [{source: 'camera', layer: 'mid', sequence: 3, keyframe: true, timestampUs: 1000, width: 1280, height: 720, payload: '0102'}]);
  assert.equal(f.call.stats().counters.sent, 1);
  await f.call.setVideo('camera', false);
  assert.equal(camera.stopped, 1);
  assert.equal(f.track.stopped, 1, 'the camera is let go with its encoder');
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
  assert.deepEqual(f.sent.slice(-1), [['leave']]);
  assert.equal(f.call.state.phase, 'idle');
  assert.equal(f.audio.stopped, 1);
  const count = f.pictures.length;
  for (const timer of f.timers) timer.callback();
  await drain();
  assert.equal(f.pictures.length, count, 'no timer publishes a picture');
});

test('a call the host ends for a reason ends with it', async () => {
  const f = fixture();
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  f.event({kind: 'ended', call: CALL, reason: 'not admitted to this workspace'});
  await drain();
  assert.equal(f.call.state.phase, 'ended');
  assert.match(f.call.state.reason, /not admitted/);
  assert.equal(f.call.state.admitted, true, 'it was in a call it lost');
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
  f.event({kind: 'heard', from: BOB, sequence: 1, timestamp48k: 0, opus: '01'});
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
  assert.ok(f.sent.some(([what]) => what === 'leave'));
});

test('peer.voice takes the page\'s planes through the wasm peer and lists its calls', async () => {
  const played = [];
  const pictures = [];
  const showing = [];
  const restarts = [];
  let onFrame;
  let onPicture;
  const wasm = {
    joined: [],
    published: [],
    voiceJoin: (workspace, call) => wasm.joined.push([workspace, call]),
    voiceLeave: () => {},
    voiceCalls: () => {},
    voiceShow: (workspace, camera, screen, muted, deafened) => wasm.shown = [camera, screen, muted, deafened],
    callAudio: (workspace, json) => wasm.published.push([workspace, JSON.parse(json)]),
    callPictures: (workspace, json) => pictures.push([workspace, JSON.parse(json)]),
    callShowing: (workspace, json) => showing.push([workspace, JSON.parse(json)]),
    callPictureRestart: (workspace, device, source) => restarts.push([workspace, device, source]),
  };
  const voice = createPeerVoice({peer: () => wasm, workspace: 'the-workspace', device: HOST, encoder: ({source, wanted, onPicture: emit}) => {
    onPicture = emit;
    return {source, wanted, keyframe() {}, stop() {}};
  }, open: async () => ({getTracks: () => [{stop() {}}]}), audio: {
    start: async capture => { onFrame = capture.onFrame; return {play: frame => played.push(frame), setMuted: async () => {}, stop: async () => {}, get captureActive() { return false; }}; },
  }});
  voice.call.join(CALL, {app: 'so.tilo.chat', record: ''});
  await drain();
  assert.deepEqual(wasm.joined, [['the-workspace', CALL]]);
  assert.deepEqual(wasm.shown, [false, false, false, false]);
  // The host's grant comes through the wasm peer's events, and its pictures
  // go out through `callPictures` as they are made.
  voice.pump(JSON.stringify([{room: 'the-workspace', kind: 'granted', grant: grantFor()}]));
  await drain();
  assert.equal(voice.call.state.phase, 'live');
  await voice.call.setVideo('camera', true);
  onPicture({source: 'camera', layer: 'mid', sequence: 1, keyframe: true, timestampUs: 0, width: 2, height: 2, payload: Uint8Array.of(7)});
  assert.deepEqual(pictures, [['the-workspace', [{source: 'camera', layer: 'mid', sequence: 1, keyframe: true, timestampUs: 0, width: 2, height: 2, payload: '07'}]]]);
  // The microphone's frames go out as the wasm peer's `callAudio`, and what
  // the plane publishes for the call is played as its `heard` events arrive.
  await voice.call.setMuted(false);
  onFrame({timestamp: 960n, duration: 960, payload: Uint8Array.of(1)});
  assert.deepEqual(wasm.published, [['the-workspace', [{timestamp48k: 960, opus: '01'}]]]);
  voice.pump(JSON.stringify([{room: 'the-workspace', kind: 'heard', from: BOB, sequence: 1, timestamp48k: 960, opus: '02'}]));
  assert.deepEqual(played.map(frame => [...frame.payload]), [[2]]);
  voice.call.showPictures([{person: BOB, source: 'camera', layer: 'mid'}]);
  assert.deepEqual(showing.at(-1), ['the-workspace', [{device: BOB, camera: ['mid'], screen: []}]]);
  assert.deepEqual(restarts, [['the-workspace', BOB, 'camera']]);
  voice.call.leave();
  assert.equal(voice.call.state.phase, 'idle');
  voice.dispose();
});
