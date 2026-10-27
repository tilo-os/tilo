import test from 'node:test';
import assert from 'node:assert/strict';
import {callIdOf, createVoiceControls, createMediaPump, endedIssue, rostersOf, revisionOf, speakingOf} from './voice-controls.mjs';
const CALL = 'c'.repeat(64);
function fixture() {
  const operations = [];
  const state = {phase: 'idle', call: null, muted: true, inputId: null, participants: []};
  const call = {state, join(id, options) { operations.push(['join', id, options]); Object.assign(state, {phase: 'joining', call: id, muted: options.muted}); },
    leave() {operations.push(['leave']); Object.assign(state, {phase: 'idle', call: null, participants: []});},
    async setMuted(value) {operations.push(['mute', value]); state.muted = value;},
    async selectInput(id) {state.inputId = id;}, inputs: async () => [], stats: () => ({captureActive: false})};
  const opened = [];
  const voice = {call, calls: [], list: async () => [{app: 'so.tilo.chat', record: '', channel: 'standup', call: CALL, active: true}],
    async open({channel}) { opened.push(channel); if (channel !== 'standup') throw new Error('invalid media binding'); return {call: CALL, epoch: 1, active: true}; }};
  const controls = createVoiceControls({voice, room: 'room-1', gesture: () => true});
  const request = command => ({app: 'so.tilo.chat', room: 'room-1', record: '', command: {version: 1, intent: 0, command}});
  return {voice, controls, request, operations, opened, state};
}
test('shared Chat commands join listening, mute, select an input and leave the governed call', async () => {
  const f = fixture();
  assert.equal((await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}))).state, 'joining');
  assert.deepEqual(f.operations[0], ['join', CALL, {muted: true}]);
  Object.assign(f.state, {phase: 'live', canPublish: true});
  await f.controls.execute(f.request({op: 'set_muted', muted: false}));
  assert.equal(f.state.muted, false);
  await f.controls.execute(f.request({op: 'select_input', input_id: 'mic-2'}));
  assert.equal(f.state.inputId, 'mic-2');
  assert.equal((await f.controls.execute(f.request({op: 'leave'}))).state, 'idle');
});
test('leave supersedes a delayed call lookup and cannot reopen capture', async () => {
  const f = fixture();
  let opened;
  f.voice.open = () => new Promise(resolve => {opened = resolve;});
  const joining = f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  await f.controls.execute(f.request({op: 'leave'}));
  opened({call: CALL, epoch: 1, active: true});
  await assert.rejects(joining, /superseded/);
  assert.deepEqual(f.operations, [['leave']]);
});
test('foreign routes, absent calls and unmute without activation fail closed', async () => {
  const f = fixture();
  await assert.rejects(f.controls.execute({...f.request({op: 'status'}), room: 'other'}), /route/);
  const refused = await f.controls.execute(f.request({op: 'join', channel_id: 'absent'}));
  assert.equal(refused.state, 'ended');
  assert.equal(refused.issue, 'unavailable');
  const controls = createVoiceControls({voice: f.voice, room: 'room-1', gesture: () => false});
  await assert.rejects(controls.execute(f.request({op: 'set_muted', muted: false})), /gesture/);
  assert.equal(f.state.muted, true);
});

test('a call the host cannot open, or never grants, ends with why; joining again clears it', async () => {
  const f = fixture();
  f.voice.open = async () => { throw new Error('exact admitted Chat is encrypted'); };
  const refused = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(refused.state, 'ended');
  assert.equal(refused.issue, 'unavailable');
  assert.equal(refused.capture_active, false);
  assert.equal((await f.controls.execute(f.request({op: 'status'}))).issue, 'unavailable', 'the reason stays until the person acts');
  f.voice.open = async () => { throw new Error('not a member of the room'); };
  assert.equal((await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}))).issue, 'denied');
  f.voice.open = async () => ({call: CALL, epoch: 1, active: true});
  const joining = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(joining.state, 'joining');
  assert.equal(joining.issue, null);
  Object.assign(f.state, {phase: 'ended', admitted: false, reason: 'no grant arrived in time'});
  assert.equal((await f.controls.execute(f.request({op: 'status'}))).issue, 'unavailable');
  assert.equal((await f.controls.execute(f.request({op: 'leave'}))).state, 'idle');
});
test('an ended call says whether it was refused or just ended', () => {
  assert.equal(endedIssue({admitted: true, reason: 'the call has ended'}), 'session_ended');
  assert.equal(endedIssue({admitted: false, reason: 'not a member of the room'}), 'denied');
  assert.equal(endedIssue({admitted: false, reason: 'this relay serves no voice carrier'}), 'unavailable');
});

test('an active call polls the retained controls even when the canvas stops redrawing, and close removes the timer', async () => {
  const f = fixture();
  f.state.phase = 'live';
  let listener;
  f.voice.call.subscribe = next => {listener = next; return () => {listener = undefined;};};
  let scheduled;
  let cleared = false;
  let polls = 0;
  const clock = {setTimeout: next => {scheduled = next; return 1;}, clearTimeout: () => {cleared = true;}};
  const pump = createMediaPump({voice: f.voice, room: 'room-1', id: 1n, active: () => true, clock,
    runtime: {take_media_requests: () => {polls++; return '[]';}}});
  pump.pump();
  assert.equal(polls, 1);
  assert.equal(typeof scheduled, 'function', 'live calls keep receiving shared UI status');
  scheduled();
  assert.equal(polls, 2);
  pump.close();
  assert.equal(cleared, true);
  assert.equal(listener, undefined);
});


test('two shared Chat surfaces cannot mute or leave one another’s call', async () => {
  const f = fixture();
  await f.controls.execute({...f.request({op: 'join', channel_id: 'standup'}), instance: '1'});
  const other = {...f.request({op: 'leave'}), instance: '2'};
  await assert.rejects(f.controls.execute(other), /own/);
  f.controls.release(other);
  assert.equal(f.state.phase, 'joining');
  f.controls.release({...other, instance: '1'});
  assert.equal(f.state.phase, 'idle');
});

test('another retained route cannot display the owner’s live call as its own', async () => {
  const f = fixture();
  await f.controls.execute({...f.request({op: 'join', channel_id: 'standup'}), instance: '1'});
  Object.assign(f.state, {phase: 'live', canPublish: true});
  assert.equal((await f.controls.execute({...f.request({op: 'status'}), instance: '1'})).state, 'live');
  const other = await f.controls.execute({...f.request({op: 'status'}), instance: '2'});
  assert.equal(other.state, 'idle');
  assert.equal(other.muted, true);
  assert.equal(other.capture_active, false);
});

test('joining from a new retained owner first leaves the prior participation', async () => {
  const f = fixture();
  await f.controls.execute({...f.request({op: 'join', channel_id: 'standup'}), instance: '1'});
  Object.assign(f.state, {phase: 'live', muted: false});
  await f.controls.execute({...f.request({op: 'join', channel_id: 'standup'}), instance: '2'});
  assert.deepEqual(f.operations.slice(1), [['leave'], ['join', CALL, {muted: true}]]);
});

test('a reused instance cannot inherit or retire another component lifetime', async () => {
  const f = fixture();
  const command = f.request({op: 'join', channel_id: 'standup'});
  await f.controls.execute({...command, instance: '1', owner: 'old-lifetime'});
  await f.controls.execute({...command, instance: '1', owner: 'replacement-lifetime'});
  assert.deepEqual(f.operations.slice(1), [['leave'], ['join', CALL, {muted: true}]]);
  f.controls.release({...command, instance: '1', owner: 'old-lifetime'});
  assert.equal(f.state.phase, 'joining');
  f.controls.release({...command, instance: '1', owner: 'replacement-lifetime'});
  assert.equal(f.state.phase, 'idle');
});

test('the mounted pump releases a retired component inside a still-open desktop', async () => {
  const f = fixture();
  f.voice.call.subscribe = () => () => {};
  const request = {...f.request({op: 'join', channel_id: 'standup'}), ticket: 1, owner: 'retained-chat'};
  let batch = [request];
  const clock = {setTimeout: () => 1, clearTimeout() {}};
  const runtime = {take_media_requests() {const result = batch; batch = []; return JSON.stringify(result);}, complete_media_request() {return true;}};
  const pump = createMediaPump({voice: f.voice, room: 'room-1', id: 1n, active: () => true, clock, runtime});
  pump.pump();
  for (let i = 0; i < 10; i++) await Promise.resolve();
  assert.equal(f.state.phase, 'joining');
  batch = [{...request, ticket: 0, retired: true}];
  pump.pump();
  assert.equal(f.state.phase, 'idle');
  assert.deepEqual(f.operations.at(-1), ['leave']);
  pump.close();
});

test('activation alone cannot unmute before a publishing grant is live', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  await assert.rejects(f.controls.execute(f.request({op: 'set_muted', muted: false})), /grant/);
  Object.assign(f.state, {phase: 'live', canPublish: false});
  await assert.rejects(f.controls.execute(f.request({op: 'set_muted', muted: false})), /grant/);
  assert.equal(f.state.muted, true);
  assert.equal(f.operations.filter(op => op[0] === 'mute').length, 0);
});

test('joining a voice channel opens its call muted, every time, and asks the browser for nothing', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.deepEqual(f.opened, ['standup'], 'nobody has to start the call first');
  assert.deepEqual(f.operations[0], ['join', CALL, {muted: true}]);
  Object.assign(f.state, {phase: 'live', canPublish: true});
  await f.controls.execute(f.request({op: 'set_muted', muted: false}));
  await f.controls.execute(f.request({op: 'leave'}));
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.deepEqual(f.operations.at(-1), ['join', CALL, {muted: true}]);
});

test('a refused microphone answers with the call on, muted, and why', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  Object.assign(f.state, {phase: 'live', canPublish: true});
  // The call refuses as voice.mjs does: still live, muted, with the reason.
  f.voice.call.setMuted = async () => { Object.assign(f.state, {muted: true, micIssue: 'permission_denied'}); };
  const status = await f.controls.execute(f.request({op: 'set_muted', muted: false}));
  assert.equal(status.state, 'live');
  assert.equal(status.muted, true);
  assert.equal(status.capture_active, false);
  assert.equal(status.issue, 'permission_denied');
});

test('a voice channel shows who is in its call before joining, each person once', async () => {
  const f = fixture();
  const ana = 'a'.repeat(64);
  const ben = 'b'.repeat(64);
  f.voice.calls = [
    {app: 'so.tilo.chat', record: '', channel: 'voice', call: CALL, active: true,
      participants: [{user: ben, device: '1'}, {user: ana, device: '2'}, {user: ana, device: '3'}]},
    {app: 'so.tilo.chat', record: '', channel: 'empty', call: CALL, active: true, participants: []},
    {app: 'so.tilo.chat', record: '', channel: 'ended', call: CALL, active: false, participants: [{user: ana}]},
    {app: 'so.tilo.chat', record: 'other', channel: 'voice', call: CALL, active: true, participants: [{user: ana}]},
  ];
  const status = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(status.rosters, [{channel_id: 'voice', people: [ana, ben]}]);
  assert.deepEqual(rostersOf(f.voice.calls, 'so.tilo.chat', 'other'), [{channel_id: 'voice', people: [ana]}]);
});

test('a watch answers when someone joins the room\'s call, and holds while nothing changes', async () => {
  const ana = 'a'.repeat(64);
  const voice = {call: {state: {phase: 'idle', participants: []}, stats: () => ({})}, calls: [], list: async () => []};
  let looks = 0;
  const controls = createVoiceControls({voice, room: 'room-1', wait: async () => {
    looks++;
    // Ana joins the standup while the watch is held.
    if (looks === 3) voice.calls = [{app: 'so.tilo.chat', record: '', channel: 'standup', call: CALL, active: true, participants: [{user: ana}]}];
  }});
  const request = command => ({app: 'so.tilo.chat', room: 'room-1', record: '', command: {version: 1, intent: 0, command}});
  const seen = await controls.execute(request({op: 'status'}));
  assert.equal(seen.revision, revisionOf(seen));
  const answered = await controls.execute(request({op: 'watch', revision: seen.revision}));
  assert.equal(looks, 3, 'held until the call changed');
  assert.deepEqual(answered.rosters, [{channel_id: 'standup', people: [ana]}]);
  assert.notEqual(answered.revision, seen.revision);
  const stale = await controls.execute(request({op: 'watch', revision: 1}));
  assert.equal(stale.revision, answered.revision, 'an out-of-date watcher is answered at once');
  assert.equal(looks, 3);
});

test('the camera and a shared screen turn on in a live call from a gesture, and the room says who shows what', async () => {
  const f = fixture();
  const video = [];
  f.voice.call.setVideo = async (source, on) => { video.push([source, on]); f.state[source] = on; };
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  // Not before the call is live.
  await assert.rejects(f.controls.execute(f.request({op: 'set_camera', on: true})), /live/);
  Object.assign(f.state, {phase: 'live', canPublish: true});
  const on = await f.controls.execute(f.request({op: 'set_camera', on: true}));
  assert.deepEqual(video, [['camera', true]]);
  assert.equal(on.camera, true);
  assert.equal(on.screen, undefined, 'written only when set');
  await f.controls.execute(f.request({op: 'set_screen', on: true}));
  await f.controls.execute(f.request({op: 'set_camera', on: false}));
  assert.deepEqual(video.slice(1), [['screen', true], ['camera', false]]);
  const controls = createVoiceControls({voice: f.voice, room: 'room-1', gesture: () => false});
  await assert.rejects(controls.execute(f.request({op: 'set_screen', on: true})), /gesture/);

  const ana = 'a'.repeat(64);
  const ben = 'b'.repeat(64);
  f.voice.calls = [{app: 'so.tilo.chat', record: '', channel: 'standup', call: CALL, active: true,
    participants: [{user: ana, camera: true}, {user: ben, screen: true}]}];
  assert.deepEqual(rostersOf(f.voice.calls, 'so.tilo.chat', ''), [{channel_id: 'standup', people: [ana, ben], cameras: [ana], screens: [ben]}], 'muted is written only when someone is');
  f.voice.calls[0].participants[1].muted = true;
  assert.deepEqual(rostersOf(f.voice.calls, 'so.tilo.chat', ''), [{channel_id: 'standup', people: [ana, ben], cameras: [ana], screens: [ben], muted: [ben]}]);
  f.voice.calls[0].participants.push({user: ben});
  assert.equal(rostersOf(f.voice.calls, 'so.tilo.chat', '')[0].muted, undefined, 'heard on another device: not muted');
  f.voice.calls[0].participants = [{user: ana, muted: true, deafened: true}, {user: ben, muted: true}];
  assert.deepEqual(rostersOf(f.voice.calls, 'so.tilo.chat', '')[0].deafened, [ana], 'the deafened, among the muted');
  f.voice.calls[0].participants.push({user: ana, muted: true});
  assert.equal(rostersOf(f.voice.calls, 'so.tilo.chat', '')[0].deafened, undefined, 'hearing on another device: not deafened');
});

test('deafening is a command of the call: it needs one, and undeafening unmutes only from a gesture', async () => {
  const f = fixture();
  const deafens = [];
  f.voice.call.setDeafened = async (value, options) => { deafens.push([value, options]); f.state.deafened = value; if (value) f.state.muted = true; };
  await assert.rejects(f.controls.execute(f.request({op: 'set_deafened', deafened: true})), /call/);
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  Object.assign(f.state, {phase: 'live', canPublish: true});
  const deafened = await f.controls.execute(f.request({op: 'set_deafened', deafened: true}));
  assert.deepEqual([deafened.deafened, deafened.muted], [true, true]);
  const heard = await f.controls.execute(f.request({op: 'set_deafened', deafened: false}));
  assert.equal(heard.deafened, undefined, 'written only when set');
  const quiet = createVoiceControls({voice: f.voice, room: 'room-1', gesture: () => false});
  await quiet.execute(f.request({op: 'set_deafened', deafened: false}));
  assert.deepEqual(deafens.map(([value, {unmute}]) => [value, unmute]), [[true, true], [false, true], [false, false]]);
});

test('the app says what it shows and collects the pictures as they arrive', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  Object.assign(f.state, {phase: 'live', canPublish: true});
  const shown = [];
  let queued = [];
  f.voice.call.showPictures = pictures => shown.push(pictures);
  f.voice.call.takePictures = after => {
    const pictures = queued;
    queued = [];
    return {pictures, cursor: after + pictures.length};
  };
  f.voice.call.waitPictures = async () => { queued = [{person: 'a'.repeat(64), stream: 's', source: 'camera', object: 'AQABAf+A'}]; };
  const pictures = [{person: 'a'.repeat(64), source: 'camera', layer: 'mid'}];
  await f.controls.execute(f.request({op: 'show', pictures}));
  assert.deepEqual(shown, [pictures]);
  const answer = await f.controls.execute(f.request({op: 'pictures', after: 0}));
  assert.equal(answer.pictures.length, 1);
  assert.equal(answer.pictures_cursor, 1);
  assert.equal(answer.state, 'live');
});

test('a bridged call names the people it carries in as guests, never as the bridge\'s person', async () => {
  const f = fixture();
  const ana = 'a'.repeat(64);
  const bridge = 'b'.repeat(64);
  const own = {service: 'discord', account: null, name: 'Tilo', avatar: null, linked: null};
  const dana = {service: 'discord', account: '80351110224678912', name: 'Dana', avatar: 'd'.repeat(64), linked: null};
  const ben = {service: 'discord', account: '90351110224678912', name: 'Ben', avatar: null, linked: ana};
  f.voice.calls = [
    {app: 'so.tilo.chat', record: '', channel: 'voice', call: CALL, active: true, participants: [
      {user: ana, device: '1'},
      {user: bridge, device: '2', bridged: dana},
      // Muted in Discord: everyone sees it.
      {user: bridge, device: '2', bridged: ben, muted: true},
      {user: bridge, device: '2', bridged: own},
    ]},
    // The bridge listening on its own, before anyone joins.
    {app: 'so.tilo.chat', record: '', channel: 'lounge', call: CALL, active: true, participants: [{user: bridge, device: '2', bridged: own}]},
  ];
  const status = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(status.rosters, [
    {channel_id: 'lounge', people: [], bridges: ['discord']},
    {channel_id: 'voice', people: [ana], bridges: ['discord'], guests: [
      {id: 'discord:80351110224678912', name: 'Dana', service: 'discord', avatar: 'd'.repeat(64)},
      {id: 'discord:90351110224678912', name: 'Ben', service: 'discord', linked: ana, muted: true},
    ]},
  ]);
  assert.equal(callIdOf({user: bridge, bridged: own}), null, 'the bridge itself is nobody in the call');
});

test('the status names who the call hears talking, people and guests by id', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  const ana = 'a'.repeat(64);
  const bridge = 'b'.repeat(64);
  const participants = [
    {user: ana, self: true, speaking: true},
    {user: ana, speaking: true},
    {user: bridge, speaking: true, bridged: {service: 'discord', account: '7', name: 'Dana'}},
    {user: bridge, speaking: true, bridged: {service: 'discord', account: null, name: 'Tilo'}},
    {user: 'c'.repeat(64), speaking: false},
  ];
  assert.deepEqual(speakingOf(participants), [ana, 'discord:7']);
  Object.assign(f.state, {phase: 'live', canPublish: true, participants});
  assert.deepEqual((await f.controls.execute(f.request({op: 'status'}))).speaking, [ana, 'discord:7']);
  Object.assign(f.state, {phase: 'idle'});
  assert.deepEqual((await f.controls.execute(f.request({op: 'status'}))).speaking, [], 'nobody talks outside a call');
});

test('a participant\'s volume reaches this device\'s playback only', async () => {
  const f = fixture();
  const volumes = [];
  f.voice.call.setVolume = (id, gain) => volumes.push([id, gain]);
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  await f.controls.execute(f.request({op: 'set_volume', participant: 'discord:7', percent: 0}));
  await f.controls.execute(f.request({op: 'set_volume', participant: 'a'.repeat(64), percent: 150}));
  assert.deepEqual(volumes, [['discord:7', 0], ['a'.repeat(64), 1.5]]);
});

test('the status names who the call hears talking, so the app can ring them', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  const [ana, ben] = ['a'.repeat(64), 'b'.repeat(64)];
  Object.assign(f.state, {phase: 'live', canPublish: true, participants: [
    {namespace: `${CALL}/${'1'.repeat(64)}`, user: ben, self: false, publishes: true, speaking: true},
    {namespace: `${CALL}/${'2'.repeat(64)}`, user: ana, self: true, publishes: true, speaking: true},
    {namespace: `${CALL}/${'3'.repeat(64)}`, user: ben, self: false, publishes: true, speaking: true},
    {namespace: `${CALL}/${'4'.repeat(64)}`, user: 'c'.repeat(64), self: false, publishes: true, speaking: false},
  ]});
  const talking = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(talking.speaking, [ana, ben], 'each person once, sorted');
  f.state.participants = f.state.participants.map(participant => ({...participant, speaking: false}));
  const quiet = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(quiet.speaking, []);
  assert.notEqual(quiet.revision, talking.revision, 'a held watch wakes when who talks changes');
});

test('a browser that cannot carry calls is told so when it joins, and opens no call', async () => {
  const f = fixture();
  f.voice.call.unsupported = () => 'WebTransport is unavailable';
  const answer = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(answer.state, 'ended');
  assert.equal(answer.issue, 'unsupported');
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.operations, []);
  assert.equal(endedIssue({admitted: false, unsupported: true, reason: 'WebTransport is unavailable'}), 'unsupported');
});
