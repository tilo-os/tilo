import test from 'node:test';
import assert from 'node:assert/strict';

import {createVoiceCall, GRANT_DEADLINE_MS, GRANT_LOOK_MS, NO_GRANT} from './voice.mjs';
import {createPeerVoice} from './voice-peer.mjs';
import {CarrierUnsupportedError, MediaAdmissionError, carrierUnsupported} from './moq-carrier.mjs';

const CALL = 'c'.repeat(64);
const ME = 'a'.repeat(64);
const BOB = 'b'.repeat(64);
const drain = async () => { for (let index = 0; index < 10; index++) await Promise.resolve(); };

function grantFor({participation = ME, token = '01', publish = true, others = [BOB]} = {}) {
  const participants = [participation, ...others].map(id => ({namespace: `${CALL}/${id}`, device: id, user: id, publishes: true}));
  return {
    call: CALL, epoch: 1, participation, token,
    endpoint: 'https://localhost:4433/moq/voice', fingerprints: [],
    directions: {publish, subscribe: true},
    publication: publish ? `${CALL}/${participation}` : null,
    subscriptions: others.map(id => `${CALL}/${id}`),
    participants, expiresAtNs: 0,
  };
}

function fixture() {
  const sent = [];
  const shows = [];
  let listener;
  const signalling = {
    join: call => sent.push(['join', call]),
    leave: () => sent.push(['leave']),
    show: (camera, screen, muted, deafened) => shows.push({camera, screen, muted, deafened}),
    onEvent(next) { listener = next; return () => { listener = undefined; }; },
  };
  const audio = {started: 0, played: [], removed: [], mutes: [], deafens: [], stopped: 0};
  audio.start = async ({muted, onFrame, onLevels, onCaptureLost, onInputChanged}) => {
    audio.started++;
    audio.onFrame = onFrame;
    audio.onLevels = onLevels;
    audio.onCaptureLost = onCaptureLost;
    audio.onInputChanged = onInputChanged;
    audio.muted = muted;
    return {
      play: frame => audio.played.push(frame),
      removePublisher: id => audio.removed.push(id),
      setDeafened: value => audio.deafens.push(value),
      setMuted: async value => { if (!value && audio.refuseCapture) throw Object.assign(new Error('Permission denied'), {name: 'NotAllowedError'}); audio.mutes.push(value); audio.muted = value; },
      get captureActive() { return !audio.muted && audio.stopped === 0; },
      selectInput: async id => { audio.input = id; },
      selectOutput: async () => true,
      stop: async () => { audio.stopped++; },
    };
  };
  const carriers = [];
  const transport = {
    async connect({grant}) {
      if (transport.refuse) throw new Error('media admission denied');
      let close;
      const carrier = {
        grant, published: [], closed: new Promise(resolve => { close = resolve; }), open: true,
        publish(frame) { carrier.published.push(frame); return true; },
        subscribe({onFrame, onPublisher}) { carrier.onFrame = onFrame; carrier.onPublisher = onPublisher; return () => {}; },
        close() { if (carrier.open) { carrier.open = false; close(); } },
        stats: () => ({sent: carrier.published.length}),
      };
      carriers.push(carrier);
      return carrier;
    },
  };
  const timers = [];
  const clock = {
    now: () => clock.at,
    at: 1000,
    setTimeout: (callback, delay) => { timers.push({callback, delay}); return timers.length; },
    clearTimeout: handle => { if (timers[handle - 1]) timers[handle - 1].callback = () => {}; },
  };
  const environment = new EventTarget();
  const call = createVoiceCall({signalling, audio, transport, environment, clock});
  const event = value => listener?.(value);
  return {sent, shows, audio, carriers, transport, timers, clock, environment, call, event};
}

/** Run the grant deadline's looks on a page that runs, each one look
 * later: how many looks it took to end. */
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

test('a joined call follows its grant: the carrier publishes the microphone and plays the others', async () => {
  const {sent, audio, carriers, call, event} = fixture();
  const states = [];
  call.subscribe(state => { if (states.at(-1) !== state.phase) states.push(state.phase); });
  call.join(CALL);
  assert.deepEqual(sent, [['join', CALL]]);
  assert.equal(call.state.phase, 'joining');
  await drain();
  assert.equal(audio.started, 1, 'the audio starts within the gesture, before any grant');
  assert.equal(audio.muted, false);
  event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(carriers.length, 1);
  assert.equal(call.state.phase, 'live');
  assert.deepEqual(call.state.participants.map(p => [p.self, p.device]), [[true, ME], [false, BOB]]);
  audio.onFrame(frame(1));
  assert.equal(carriers[0].published.length, 1);
  carriers[0].onFrame({publisher: BOB, sequence: 0n, ...frame(2)});
  assert.equal(audio.played[0].publisherId, BOB);
  assert.deepEqual(states, ['joining', 'connecting', 'live']);
});

test('muting stops publication; a grant that may not publish mutes and says so', async () => {
  const {audio, carriers, call, event} = fixture();
  call.join(CALL);
  await drain();
  event({kind: 'granted', grant: grantFor()});
  await drain();
  await call.setMuted(true);
  assert.deepEqual(audio.mutes, [true]);
  audio.onFrame(frame(1));
  assert.equal(carriers[0].published.length, 0, 'nothing leaves while muted');
  await call.setMuted(false);
  audio.onFrame(frame(2));
  assert.equal(carriers[0].published.length, 1);
  // The owner denies this person's voice: the new grant replaces the carrier.
  event({kind: 'granted', grant: grantFor({publish: false, token: '02'})});
  await drain();
  assert.equal(call.state.muted, true);
  assert.equal(call.state.canPublish, false);
  assert.equal(carriers.length, 2);
  assert.equal(carriers[0].open, false);
  audio.onFrame(frame(3));
  assert.equal(carriers[1].published.length, 0);
  await call.setMuted(false);
  assert.equal(call.state.muted, true, 'cannot unmute without the right to publish');
});

test('a roster change or a renewed token keeps the carrier; a dropped carrier reopens with the newest token', async () => {
  const {carriers, call, event, timers} = fixture();
  call.join(CALL);
  await drain();
  event({kind: 'granted', grant: grantFor()});
  await drain();
  event({kind: 'granted', grant: grantFor({others: [BOB, 'd'.repeat(64)]})});
  await drain();
  assert.equal(carriers.length, 1, 'new participants are heard on the same carrier');
  assert.equal(call.state.participants.length, 3);
  event({kind: 'granted', grant: grantFor({token: '03'})});
  await drain();
  assert.equal(carriers.length, 1, 'the admitted carrier outlives the token that admitted it');
  assert.equal(carriers[0].open, true);
  carriers[0].close();
  await drain();
  assert.equal(call.state.phase, 'reconnecting');
  assert.equal(timers.at(-1).delay, 250);
  timers.at(-1).callback();
  await drain();
  assert.equal(carriers.length, 2);
  assert.equal(carriers[1].grant.token, '03', 'a new carrier presents the renewed token');
  assert.equal(call.state.phase, 'live');
});

test('a network change reopens the carrier at once; refusals back off', async () => {
  const {carriers, call, event, environment, transport, timers} = fixture();
  call.join(CALL);
  await drain();
  event({kind: 'granted', grant: grantFor()});
  await drain();
  environment.dispatchEvent(new Event('online'));
  await drain();
  assert.equal(carriers.length, 2);
  assert.equal(carriers[0].open, false);
  transport.refuse = true;
  carriers[1].close();
  await drain();
  timers.at(-1).callback();
  await drain();
  assert.deepEqual(timers.map(timer => timer.delay).filter(delay => delay !== GRANT_LOOK_MS), [250, 1000]);
});

test('speaking follows the measured levels, held briefly', async () => {
  const {audio, call, event, clock} = fixture();
  call.join(CALL);
  await drain();
  event({kind: 'granted', grant: grantFor()});
  await drain();
  audio.onLevels({capture: 0.2, publishers: new Map([[BOB, 0]])});
  assert.deepEqual(call.state.participants.map(p => p.speaking), [true, false]);
  clock.at += 100;
  audio.onLevels({capture: 0, publishers: new Map([[BOB, 0.1]])});
  assert.deepEqual(call.state.participants.map(p => p.speaking), [true, true]);
  clock.at += 500;
  audio.onLevels({capture: 0, publishers: new Map([[BOB, 0]])});
  assert.deepEqual(call.state.participants.map(p => p.speaking), [false, false]);
});

test('leaving ends the participation; an ended call stops everything', async () => {
  const first = fixture();
  first.call.join(CALL);
  await drain();
  first.event({kind: 'granted', grant: grantFor()});
  await drain();
  first.call.leave();
  await drain();
  assert.deepEqual(first.sent, [['join', CALL], ['leave']]);
  assert.equal(first.carriers[0].open, false);
  assert.equal(first.audio.stopped, 1);
  assert.equal(first.call.state.phase, 'idle');

  const second = fixture();
  second.call.join(CALL);
  await drain();
  second.event({kind: 'granted', grant: grantFor()});
  await drain();
  second.event({kind: 'ended', call: CALL, reason: 'the call has ended'});
  await drain();
  assert.equal(second.call.state.phase, 'ended');
  assert.equal(second.call.state.reason, 'the call has ended');
  assert.equal(second.carriers[0].open, false);
  assert.equal(second.audio.stopped, 1);
  second.event({kind: 'granted', grant: grantFor({token: '09'})});
  await drain();
  assert.equal(second.carriers.length, 1, 'a late grant reopens nothing');
});

test('a join the host never grants ends at the deadline with a reason; a grant in time keeps it', async () => {
  const late = fixture();
  late.call.join(CALL);
  await drain();
  assert.ok(late.timers.some(timer => timer.delay === GRANT_LOOK_MS), 'a join waits for its grant with a deadline');
  assert.equal(runDeadline(late), Math.ceil(GRANT_DEADLINE_MS / GRANT_LOOK_MS), 'the deadline is the time the page ran');
  await drain();
  assert.equal(late.call.state.phase, 'ended');
  assert.equal(late.call.state.reason, NO_GRANT);
  assert.equal(late.call.state.admitted, false);
  assert.deepEqual(late.sent, [['join', CALL], ['leave']], 'the participation is let go');
  assert.equal(late.audio.stopped, 1, 'nothing keeps the microphone');

  const granted = fixture();
  granted.call.join(CALL);
  await drain();
  granted.event({kind: 'granted', grant: grantFor()});
  await drain();
  granted.timers.find(timer => timer.delay === GRANT_LOOK_MS).callback();
  await drain();
  assert.equal(granted.call.state.phase, 'live');
  assert.equal(granted.call.state.admitted, true);
  granted.event({kind: 'ended', call: CALL, reason: 'the call has ended'});
  await drain();
  assert.equal(granted.call.state.admitted, true, 'a granted call that ends was not refused');

  const refused = fixture();
  refused.call.join(CALL);
  await drain();
  refused.event({kind: 'ended', call: CALL, reason: 'not a member of the room'});
  await drain();
  assert.equal(refused.call.state.phase, 'ended');
  assert.equal(refused.call.state.admitted, false);
});

test('a call whose carrier lost its admission waits out an offline spell for the fresh grant', async () => {
  const offline = fixture();
  offline.environment.navigator = {onLine: true};
  offline.call.join(CALL);
  await drain();
  offline.event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(offline.call.state.phase, 'live');
  // The session lost the host while the carrier still ran: the host forgot
  // the participation and refuses its token.
  offline.environment.navigator.onLine = false;
  const working = offline.transport.connect;
  offline.transport.connect = async () => { throw new MediaAdmissionError(); };
  offline.carriers[0].close();
  await drain();
  offline.timers.findLast(timer => timer.delay > 0 && timer.delay !== GRANT_LOOK_MS)?.callback();
  await drain();
  assert.equal(offline.call.state.phase, 'reconnecting');
  // Offline for twice the deadline: no grant could come, the call waits.
  for (let index = 0; index < 2 * GRANT_DEADLINE_MS / GRANT_LOOK_MS; index++) {
    offline.clock.at += GRANT_LOOK_MS;
    offline.timers.findLast(timer => timer.delay === GRANT_LOOK_MS).callback();
  }
  await drain();
  assert.equal(offline.call.state.phase, 'reconnecting');
  assert.deepEqual(offline.sent, [['join', CALL]], 'the participation is kept');
  // Back online, the session joins afresh and the host grants again.
  offline.environment.navigator.onLine = true;
  offline.transport.connect = working;
  offline.event({kind: 'granted', grant: grantFor({participation: BOB.replace(/b/g, 'd'), token: '02'})});
  await drain();
  assert.equal(offline.call.state.phase, 'live');

  // Online, the same wait ends at the deadline.
  const online = fixture();
  online.environment.navigator = {onLine: true};
  online.call.join(CALL);
  await drain();
  online.event({kind: 'granted', grant: grantFor()});
  await drain();
  online.transport.connect = async () => { throw new MediaAdmissionError(); };
  online.carriers[0].close();
  await drain();
  online.timers.findLast(timer => timer.delay > 0 && timer.delay !== GRANT_LOOK_MS)?.callback();
  await drain();
  assert.equal(runDeadline(online), Math.ceil(GRANT_DEADLINE_MS / GRANT_LOOK_MS));
  await drain();
  assert.equal(online.call.state.phase, 'ended');
});

test('peer.voice lists, governs and signals through the wasm peer', async () => {
  const calls = [];
  const wasm = {
    voiceCalls: room => calls.push(['calls', room]),
    voiceCall: (room, operation, request) => calls.push(['call', room, JSON.parse(operation), request]),
    voiceJoin: (room, call) => calls.push(['join', room, call]),
    voiceLeave: room => calls.push(['leave', room]),
    voiceShow: (room, camera, screen, muted, deafened) => calls.push(['show', room, camera, screen, muted, deafened]),
  };
  let signalling;
  const voice = createPeerVoice({
    peer: () => wasm, room: 'room-1', random: bytes => bytes.fill(7),
    createCall: options => { signalling = options.signalling; return {dispose() {}}; },
  });
  const listing = voice.list();
  voice.pump(JSON.stringify([
    {kind: 'calls', room: 'other', calls: []},
    {kind: 'calls', room: 'room-1', calls: [{call: CALL, epoch: 2, active: true, participants: []}]},
  ]));
  assert.deepEqual((await listing).map(call => call.call), [CALL]);
  const ending = voice.end({call: CALL, epoch: 2});
  assert.deepEqual(calls.at(-1), ['call', 'room-1', {kind: 'end', call: CALL, epoch: 2}, '07'.repeat(16)]);
  voice.pump(JSON.stringify([{kind: 'callResult', room: 'room-1', request: '07'.repeat(16), call: {call: CALL, epoch: 3, active: false}}]));
  assert.equal((await ending).active, false);
  const refused = voice.restart({call: CALL, epoch: 3});
  voice.pump(JSON.stringify([{kind: 'callResult', room: 'room-1', request: '07'.repeat(16), error: 'only the owner'}]));
  await assert.rejects(refused, /only the owner/);
  const events = [];
  signalling.onEvent(event => events.push(event.kind));
  signalling.join(CALL);
  signalling.show(true, false, true, true);
  assert.deepEqual(calls.at(-1), ['show', 'room-1', true, false, true, true]);
  signalling.leave();
  voice.pump(JSON.stringify([{kind: 'granted', room: 'room-1', grant: grantFor()}, {kind: 'ended', room: 'room-1', call: CALL, reason: 'x'}]));
  assert.deepEqual(events, ['granted', 'ended']);
  assert.deepEqual(calls.slice(-3), [['join', 'room-1', CALL], ['show', 'room-1', true, false, true, true], ['leave', 'room-1']]);
  voice.dispose();
});


test('a late audio start after leave cannot replace the next call audio or keep its microphone', async () => {
  const f = fixture();
  const starts = [];
  f.audio.start = options => new Promise(resolve => starts.push({options, resolve}));
  f.call.join(CALL);
  f.call.leave();
  assert.equal(starts[0].options.signal.aborted, true, 'leaving cancels acquisition immediately');
  f.call.join('d'.repeat(64));
  let oldStops = 0;
  starts[0].resolve({stop: async () => { oldStops++; }, setMuted: async () => {}});
  await drain();
  assert.equal(oldStops, 1, 'the previous call must release its microphone');
  let newStops = 0;
  starts[1].resolve({stop: async () => { newStops++; }, setMuted: async () => {}});
  await drain();
  f.call.leave();
  await drain();
  assert.equal(newStops, 1);
});

test('audio acquisition failure ends the participation instead of claiming a live call', async () => {
  const f = fixture();
  f.audio.start = async () => { throw new Error('Microphone denied'); };
  f.call.join(CALL);
  await drain();
  assert.equal(f.call.state.phase, 'ended');
  assert.match(f.call.state.reason, /Microphone denied/);
  assert.deepEqual(f.sent, [['join', CALL], ['leave']]);
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(f.carriers.length, 0);
});


test('a terminal audio failure cannot leave a live carrier or microphone status', async () => {
  const f = fixture();
  f.audio.start = async options => {
    f.audio.fail = options.onError;
    return {setMuted: async () => {}, stop: async () => {f.audio.stopped++;}};
  };
  f.call.join(CALL);
  await drain();
  f.event({kind: 'granted', grant: grantFor()});
  await drain();
  assert.equal(f.call.state.phase, 'live');
  f.audio.fail(new Error('Audio worker failed'));
  await drain();
  assert.equal(f.call.state.phase, 'ended');
  assert.equal(f.audio.stopped, 1);
  assert.equal(f.carriers[0].open, false);
  assert.deepEqual(f.sent.at(-1), ['leave']);
});

test('resetting the certified identity clears cached calls and rejects old pending controls', async () => {
  let left = 0;
  const voice = createPeerVoice({peer: () => ({voiceCalls() {}, voiceCall() {}}), room: 'room-1', random: bytes => bytes.fill(7),
    createCall: () => ({leave() {left++;}, dispose() {}})});
  voice.pump(JSON.stringify([{kind: 'calls', room: 'room-1', calls: [{call: CALL}]}]));
  assert.equal(voice.calls.length, 1);
  const listing = assert.rejects(voice.list(), /identity changed/);
  const ending = assert.rejects(voice.end({call: CALL, epoch: 1}), /identity changed/);
  voice.reset();
  await Promise.all([listing, ending]);
  assert.equal(voice.calls.length, 0);
  assert.equal(left, 1);
  voice.dispose();
});


test('reconnecting pauses physical capture and resumes the prior local intent when live', async () => {
  const f = fixture();
  f.call.join(CALL); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  assert.equal(f.call.stats().captureActive, true);
  f.carriers[0].close(); await drain();
  assert.equal(f.call.state.phase, 'reconnecting');
  assert.equal(f.call.stats().captureActive, false);
  f.timers.at(-1).callback(); await drain();
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.call.stats().captureActive, true);
  f.call.dispose();
});

test('a mute during reconnect cannot be undone by reopening the carrier', async () => {
  const f = fixture();
  f.call.join(CALL); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  f.carriers[0].close(); await drain();
  assert.equal(f.call.stats().captureActive, false);
  await f.call.setMuted(true);
  f.timers.at(-1).callback(); await drain();
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.stats().captureActive, false);
  f.call.dispose();
});

test('a microphone refused on reconnect keeps the call, muted, and says why', async () => {
  const f = fixture();
  f.call.join(CALL); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  f.carriers[0].close(); await drain();
  f.audio.refuseCapture = true;
  f.timers.at(-1).callback(); await drain();
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.micIssue, 'permission_denied');
  assert.equal(f.call.stats().captureActive, false);
  assert.notDeepEqual(f.sent.at(-1), ['leave']);
  f.call.dispose();
});

test('the host hears whenever this person is muted: on join, muting, a lost microphone and a grant that may not publish', async () => {
  const f = fixture();
  f.call.join(CALL, {muted: true}); await drain();
  assert.deepEqual(f.shows, [{camera: false, screen: false, muted: true, deafened: false}], 'a muted joiner says so before its grant');
  const others = {...grantFor(), participants: grantFor().participants.map(p => (p.user === BOB ? {...p, muted: true} : p))};
  f.event({kind: 'granted', grant: others}); await drain();
  assert.deepEqual(f.call.state.participants.map(p => [p.user, p.muted]), [[ME, false], [BOB, true]], 'each participant carries whether it is muted');
  assert.equal(f.shows.length, 1, 'a grant that changes nothing says nothing');
  await f.call.setMuted(false);
  assert.deepEqual(f.shows.at(-1), {camera: false, screen: false, muted: false, deafened: false});
  f.audio.onCaptureLost(new DOMException('Requested device not found', 'NotFoundError'));
  await drain();
  assert.deepEqual(f.shows.at(-1), {camera: false, screen: false, muted: true, deafened: false}, 'a lost microphone mutes for everyone');
  await f.call.setMuted(false);
  assert.equal(f.shows.at(-1).muted, false);
  f.event({kind: 'granted', grant: grantFor({publish: false, token: '02'})}); await drain();
  assert.equal(f.shows.at(-1).muted, true, 'a grant that may not publish mutes for everyone');
  const told = f.shows.length;
  f.call.leave();
  await f.call.setMuted(false);
  assert.equal(f.shows.length, told, 'out of the call, nothing is said');
  f.call.dispose();
});

test('deafening silences the call and mutes; undeafening puts the microphone back as it was', async () => {
  const f = fixture();
  f.call.join(CALL, {muted: true}); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  await f.call.setMuted(false);
  await f.call.setDeafened(true);
  assert.equal(f.call.state.deafened, true);
  assert.equal(f.call.state.muted, true, 'a deafened person is muted');
  assert.deepEqual(f.audio.deafens.at(-1), true, 'playback is silenced');
  assert.equal(f.audio.muted, true, 'the microphone stops');
  assert.deepEqual(f.shows.at(-1), {camera: false, screen: false, muted: true, deafened: true}, 'everyone sees it');
  f.audio.onFrame(frame(1));
  assert.equal(f.carriers[0].published.length, 0, 'nothing leaves while deafened');
  await f.call.setDeafened(false);
  assert.deepEqual([f.call.state.deafened, f.call.state.muted], [false, false], 'unmuted before, unmuted again');
  assert.equal(f.audio.deafens.at(-1), false);
  assert.deepEqual(f.shows.at(-1), {camera: false, screen: false, muted: false, deafened: false});
  // Muted before deafening: muted after.
  await f.call.setMuted(true);
  await f.call.setDeafened(true);
  await f.call.setDeafened(false);
  assert.deepEqual([f.call.state.deafened, f.call.state.muted], [false, true]);
  // Without a gesture to ask for the microphone, undeafening stays muted.
  await f.call.setMuted(false);
  await f.call.setDeafened(true);
  await f.call.setDeafened(false, {unmute: false});
  assert.deepEqual([f.call.state.deafened, f.call.state.muted], [false, true]);
  // Unmuting while deafened hears the call again.
  await f.call.setDeafened(true);
  await f.call.setMuted(false);
  assert.deepEqual([f.call.state.deafened, f.call.state.muted], [false, false]);
  assert.equal(f.audio.deafens.at(-1), false);
  // A new call starts hearing.
  await f.call.setDeafened(true);
  f.call.leave();
  f.call.join(CALL, {muted: true}); await drain();
  assert.equal(f.call.state.deafened, false);
  assert.equal(f.audio.deafens.at(-1), false, 'the audio of the new call plays');
  f.call.dispose();
});

test('a refused unmute keeps the person in the call, muted, until they allow it', async () => {
  const f = fixture();
  f.call.join(CALL, {muted: true}); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  assert.equal(f.call.state.phase, 'live');
  f.audio.refuseCapture = true;
  await f.call.setMuted(false);
  assert.equal(f.call.state.phase, 'live', 'still in the call');
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.micIssue, 'permission_denied');
  assert.notDeepEqual(f.sent.at(-1), ['leave']);
  f.audio.refuseCapture = false;
  await f.call.setMuted(false);
  assert.equal(f.call.state.muted, false);
  assert.equal(f.call.state.micIssue, null);
  assert.equal(f.call.stats().captureActive, true);
  f.call.dispose();
});

test('a participant plays at the volume set for them, a guest by their id', async () => {
  const {audio, carriers, call, event} = fixture();
  const DANA = 'd'.repeat(64);
  call.join(CALL);
  await drain();
  const grant = grantFor({others: [BOB, DANA]});
  grant.participants[2].bridged = {service: 'discord', account: '7', name: 'Dana', avatar: null, linked: null};
  event({kind: 'granted', grant});
  await drain();
  assert.deepEqual(call.state.participants[2].bridged, grant.participants[2].bridged, 'who a bridge carries reaches the app');
  call.setVolume('discord:7', 0);
  call.setVolume(BOB, 1.5);
  carriers[0].onFrame({publisher: DANA, sequence: 0n, ...frame(1)});
  carriers[0].onFrame({publisher: BOB, sequence: 0n, ...frame(1)});
  call.setVolume(BOB, 1);
  carriers[0].onFrame({publisher: BOB, sequence: 1n, ...frame(2)});
  assert.deepEqual(audio.played.map(played => [played.publisherId, played.gain]), [[DANA, 0], [BOB, 1.5], [BOB, 1]]);
  call.leave();
  call.join(CALL);
  await drain();
  event({kind: 'granted', grant});
  await drain();
  carriers.at(-1).onFrame({publisher: DANA, sequence: 2n, ...frame(3)});
  assert.equal(audio.played.at(-1).gain, 1, 'a volume lasts as long as the call');
});

test('a microphone unplugged mid-call keeps the call: capture moves to the default, or mutes and says why', async () => {
  const f = fixture();
  f.call.join(CALL, {muted: true}); await drain();
  f.event({kind: 'granted', grant: grantFor()}); await drain();
  await f.call.selectInput('usb-mic');
  await f.call.setMuted(false);
  assert.equal(f.call.state.inputId, 'usb-mic');
  // The audio session picked up the default microphone in its place.
  f.audio.onInputChanged(undefined);
  assert.equal(f.call.state.inputId, null);
  assert.equal(f.call.state.phase, 'live');
  assert.equal(f.call.state.muted, false);
  // No microphone is left: muted, the reason shown, still in the call.
  f.audio.onCaptureLost(new DOMException('Requested device not found', 'NotFoundError'));
  await drain();
  assert.equal(f.call.state.phase, 'live', 'still in the call, still hearing it');
  assert.equal(f.call.state.muted, true);
  assert.equal(f.call.state.micIssue, 'source_lost');
  assert.notDeepEqual(f.sent.at(-1), ['leave']);
  // Plugged back in: unmuting picks it up again.
  await f.call.setMuted(false);
  assert.equal(f.call.state.muted, false);
  assert.equal(f.call.state.micIssue, null);
  f.call.dispose();
});

test('a browser that cannot open the carrier says so instead of reconnecting forever', async () => {
  // No WebTransport, or one too old to offer the carrier's subprotocol
  // (the host answers those 404 every time).
  assert.equal(carrierUnsupported(undefined), 'WebTransport is unavailable');
  assert.match(carrierUnsupported(class {}), /cannot negotiate/);
  assert.equal(carrierUnsupported(class { get protocol() { return ''; } }), null);

  const before = fixture();
  before.transport.unsupported = () => 'WebTransport is unavailable';
  before.call.join(CALL); await drain();
  assert.equal(before.call.state.phase, 'ended');
  assert.equal(before.call.state.unsupported, true);
  assert.deepEqual(before.sent, [], 'never joins: the others would see someone who hears nothing');
  assert.equal(before.audio.started, 0);

  const during = fixture();
  during.call.join(CALL); await drain();
  during.transport.connect = async () => { throw new CarrierUnsupportedError('WebTransport is unavailable'); };
  during.event({kind: 'granted', grant: grantFor()}); await drain();
  assert.equal(during.call.state.phase, 'ended');
  assert.equal(during.call.state.unsupported, true);
  assert.deepEqual(during.sent.at(-1), ['leave']);
  assert.equal(during.timers.filter(timer => timer.delay < GRANT_LOOK_MS).length, 0, 'no reconnect is scheduled');
});
