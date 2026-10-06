import test from 'node:test';
import assert from 'node:assert/strict';
import {createVoiceControls, createMediaPump} from './voice-controls.mjs';
const CALL = 'c'.repeat(64);
function fixture() {
  const operations = [];
  const state = {phase: 'idle', call: null, muted: true, inputId: null, participants: []};
  const call = {state, join(id, options) { operations.push(['join', id, options]); Object.assign(state, {phase: 'joining', call: id, muted: options.muted}); },
    leave() {operations.push(['leave']); Object.assign(state, {phase: 'idle', call: null, participants: []});},
    async setMuted(value) {operations.push(['mute', value]); state.muted = value;},
    async selectInput(id) {state.inputId = id;}, inputs: async () => [], stats: () => ({captureActive: false})};
  const voice = {call, list: async () => [{app: 'so.tilo.chat', record: '', channel: 'standup', call: CALL, active: true}]};
  const controls = createVoiceControls({voice, room: 'room-1', gesture: () => true});
  const request = command => ({app: 'so.tilo.chat', room: 'room-1', record: '', command: {version: 1, intent: 0, command}});
  return {voice, controls, request, operations, state};
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
  let listed;
  f.voice.list = () => new Promise(resolve => {listed = resolve;});
  const joining = f.controls.execute(f.request({op: 'join', channel_id: 'standup'}));
  await f.controls.execute(f.request({op: 'leave'}));
  listed([{app: 'so.tilo.chat', record: '', channel: 'standup', call: CALL, active: true}]);
  await assert.rejects(joining, /superseded/);
  assert.deepEqual(f.operations, [['leave']]);
});
test('foreign routes, absent calls and unmute without activation fail closed', async () => {
  const f = fixture();
  await assert.rejects(f.controls.execute({...f.request({op: 'status'}), room: 'other'}), /route/);
  await assert.rejects(f.controls.execute(f.request({op: 'join', channel_id: 'absent'})), /active call/);
  const controls = createVoiceControls({voice: f.voice, room: 'room-1', gesture: () => false});
  await assert.rejects(controls.execute(f.request({op: 'set_muted', muted: false})), /gesture/);
  assert.equal(f.state.muted, true);
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
