import test from 'node:test';
import assert from 'node:assert/strict';
import {createVoiceControls, endedIssue, rostersOf, revisionOf, speakingOf} from './voice-controls.mjs';
const CALL = 'c'.repeat(64);
function fixture() {
  const operations = [];
  const state = {phase: 'idle', call: null, muted: true, inputId: null, members: []};
  const call = {state, join(id, options) { operations.push(['join', id, options]); Object.assign(state, {phase: 'joining', call: id, muted: options.muted}); },
    leave() {operations.push(['leave']); Object.assign(state, {phase: 'idle', call: null, members: []});},
    async setMuted(value) {operations.push(['mute', value]); state.muted = value;},
    async selectInput(id) {state.inputId = id;}, inputs: async () => [], stats: () => ({captureActive: false})};
  const opened = [];
  const voice = {call, calls: [], list: async () => [{channel: 'standup', call: CALL, active: true, people: []}],
    async open({channel}) { opened.push(channel); if (channel !== 'standup') throw new Error('invalid media binding'); return {call: CALL, active: true}; }};
  const controls = createVoiceControls({voice, workspace: 'workspace-1', gesture: () => true});
  const request = command => ({app: 'so.tilo.chat', workspace: 'workspace-1', record: '', command: {version: 1, intent: 0, command}});
  return {voice, controls, request, operations, opened, state};
}
test('shared Chat commands join listening, mute, select an input and leave the governed call', async () => {
  const f = fixture();
  assert.equal((await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}))).state, 'joining');
  assert.deepEqual(f.operations[0], ['join', CALL, {muted: true, app: 'so.tilo.chat', record: ''}]);
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
  await assert.rejects(f.controls.execute({...f.request({op: 'status'}), workspace: 'other'}), /route/);
  const refused = await f.controls.execute(f.request({op: 'join', channel_id: 'absent'}));
  assert.equal(refused.state, 'ended');
  assert.equal(refused.issue, 'transport_failed');
  const controls = createVoiceControls({voice: f.voice, workspace: 'workspace-1', gesture: () => false});
  await assert.rejects(controls.execute(f.request({op: 'set_muted', muted: false})), /gesture/);
  assert.equal(f.state.muted, true);
});

test('a call the host cannot open, or never grants, ends with why; joining again clears it', async () => {
  const f = fixture();
  f.voice.open = async () => { throw new Error('exact admitted Chat is encrypted'); };
  const refused = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(refused.state, 'ended');
  assert.equal(refused.issue, 'transport_failed');
  assert.equal(refused.capture_active, false);
  assert.equal((await f.controls.execute(f.request({op: 'status'}))).issue, 'transport_failed', 'the reason stays until the person acts');
  f.voice.open = async () => { throw new Error('not a member of the workspace'); };
  assert.equal((await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}))).issue, 'denied');
  f.voice.open = async () => ({call: CALL, epoch: 1, active: true});
  const joining = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(joining.state, 'joining');
  assert.equal(joining.issue, null);
  Object.assign(f.state, {phase: 'ended', admitted: false, reason: 'no grant arrived in time'});
  assert.equal((await f.controls.execute(f.request({op: 'status'}))).issue, 'transport_failed');
  assert.equal((await f.controls.execute(f.request({op: 'leave'}))).state, 'idle');
});
test('an ended call says whether it was refused or just ended', () => {
  assert.equal(endedIssue({admitted: true, reason: 'the call has ended'}), 'session_ended');
  assert.equal(endedIssue({admitted: false, reason: 'not a member of the workspace'}), 'denied');
  // No host answer says the workspace lacks voice: every workspace has calls.
  assert.equal(endedIssue({admitted: false, reason: 'calls require a subscription to the workspace'}), 'transport_failed');
  // Only this device failing to start the call's audio is `unavailable`.
  assert.equal(endedIssue({admitted: false, audioFailed: true, reason: 'no audio output'}), 'unavailable');
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
  assert.deepEqual(f.operations.slice(1), [['leave'], ['join', CALL, {muted: true, app: 'so.tilo.chat', record: ''}]]);
});

test('a reused instance cannot inherit or retire another component lifetime', async () => {
  const f = fixture();
  const command = f.request({op: 'join', channel_id: 'standup'});
  await f.controls.execute({...command, instance: '1', owner: 'old-lifetime'});
  await f.controls.execute({...command, instance: '1', owner: 'replacement-lifetime'});
  assert.deepEqual(f.operations.slice(1), [['leave'], ['join', CALL, {muted: true, app: 'so.tilo.chat', record: ''}]]);
  f.controls.release({...command, instance: '1', owner: 'old-lifetime'});
  assert.equal(f.state.phase, 'joining');
  f.controls.release({...command, instance: '1', owner: 'replacement-lifetime'});
  assert.equal(f.state.phase, 'idle');
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
  assert.deepEqual(f.operations[0], ['join', CALL, {muted: true, app: 'so.tilo.chat', record: ''}]);
  Object.assign(f.state, {phase: 'live', canPublish: true});
  await f.controls.execute(f.request({op: 'set_muted', muted: false}));
  await f.controls.execute(f.request({op: 'leave'}));
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.deepEqual(f.operations.at(-1), ['join', CALL, {muted: true, app: 'so.tilo.chat', record: ''}]);
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

test('a voice channel shows who is in its call before joining', async () => {
  const f = fixture();
  const ana = 'a'.repeat(64);
  const ben = 'b'.repeat(64);
  f.voice.calls = [
    {channel: 'voice', call: 'voice', active: true, people: [ana, ben], muted: [ben]},
    {channel: 'empty', call: 'empty', active: true, people: []},
    {channel: 'standup', call: 'standup', active: true, people: [ana], cameras: [ana], screens: []},
  ];
  const status = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(status.rosters, [
    {channel_id: 'standup', people: [ana], cameras: [ana]},
    {channel_id: 'voice', people: [ana, ben], muted: [ben]},
  ], 'sorted by channel, and what nobody shows is not written');
});

test('a watch answers when someone joins the workspace\'s call, and holds while nothing changes', async () => {
  const ana = 'a'.repeat(64);
  const voice = {call: {state: {phase: 'idle', members: []}, stats: () => ({})}, calls: [], list: async () => []};
  let looks = 0;
  const controls = createVoiceControls({voice, workspace: 'workspace-1', wait: async () => {
    looks++;
    // Ana joins the standup while the watch is held.
    if (looks === 3) voice.calls = [{channel: 'standup', call: 'standup', active: true, people: [ana]}];
  }});
  const request = command => ({app: 'so.tilo.chat', workspace: 'workspace-1', record: '', command: {version: 1, intent: 0, command}});
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

test('the camera and a shared screen turn on in a live call from a gesture, and the workspace says who shows what', async () => {
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
  const controls = createVoiceControls({voice: f.voice, workspace: 'workspace-1', gesture: () => false});
  await assert.rejects(controls.execute(f.request({op: 'set_screen', on: true})), /gesture/);
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
  const quiet = createVoiceControls({voice: f.voice, workspace: 'workspace-1', gesture: () => false});
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

test('the status names who the call hears talking, each person by id', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  const ana = 'a'.repeat(64);
  const members = [
    {user: ana, self: true, speaking: true},
    {user: ana, speaking: true},
    {user: 'c'.repeat(64), speaking: false},
  ];
  assert.deepEqual(speakingOf(members), [ana]);
  Object.assign(f.state, {phase: 'live', canPublish: true, members});
  assert.deepEqual((await f.controls.execute(f.request({op: 'status'}))).speaking, [ana]);
  Object.assign(f.state, {phase: 'idle'});
  assert.deepEqual((await f.controls.execute(f.request({op: 'status'}))).speaking, [], 'nobody talks outside a call');
});

test('a member\'s volume reaches this device\'s playback only', async () => {
  const f = fixture();
  const volumes = [];
  f.voice.call.setVolume = (id, gain) => volumes.push([id, gain]);
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  await f.controls.execute(f.request({op: 'set_volume', member: 'b'.repeat(64), percent: 0}));
  await f.controls.execute(f.request({op: 'set_volume', member: 'a'.repeat(64), percent: 150}));
  assert.deepEqual(volumes, [['b'.repeat(64), 0], ['a'.repeat(64), 1.5]]);
});

test('the status names who the call hears talking, so the app can ring them', async () => {
  const f = fixture();
  await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  const [ana, ben] = ['a'.repeat(64), 'b'.repeat(64)];
  Object.assign(f.state, {phase: 'live', canPublish: true, members: [
    {user: ben, self: false, publishes: true, speaking: true},
    {user: ana, self: true, publishes: true, speaking: true},
    {user: ben, self: false, publishes: true, speaking: true},
    {user: 'c'.repeat(64), self: false, publishes: true, speaking: false},
  ]});
  const talking = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(talking.speaking, [ana, ben], 'each person once, sorted');
  assert.deepEqual(talking.publishers, [ben, 'c'.repeat(64)], 'each person whose tile carries a stream once');
  assert.equal(talking.publication, ana, 'this device publishes under its person');
  f.state.members = f.state.members.map(member => ({...member, speaking: false}));
  const quiet = await f.controls.execute(f.request({op: 'status'}));
  assert.deepEqual(quiet.speaking, []);
  assert.notEqual(quiet.revision, talking.revision, 'a held watch wakes when who talks changes');
});

test('a browser that cannot carry calls is told so when it joins, and opens no call', async () => {
  const f = fixture();
  f.voice.call.unsupported = () => 'the call media queue is unavailable';
  const answer = await f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  assert.equal(answer.state, 'ended');
  assert.equal(answer.issue, 'unsupported');
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.operations, []);
  assert.equal(endedIssue({admitted: false, unsupported: true, reason: 'the call media queue is unavailable'}), 'unsupported');
});
