import test from 'node:test';
import assert from 'node:assert/strict';
import {createNativeCatalogueBridge, invokeRoomMembership, roomGovernanceResource, runRoomAction} from './native-catalogue.mjs';
/** The shared hang bound: a wait this long means the code under test is stuck. */
const HANG_BOUND_MS = Number(process.env.TILO_TEST_HANG_BOUND_SECS || 120) * 1000;
async function until(check,what){
  const deadline=Date.now()+HANG_BOUND_MS;
  while(!check()){
    if(Date.now()>deadline)throw new Error(`timed out waiting for ${what}`);
    await new Promise(done=>setImmediate(done));
  }
}

test('the desktop asks the room host to run a room record action by its app and action alone', async () => {
  const calls = [];
  const connection = {invoke(target, argument) {
    calls.push({target, argument});
    const update = target.address.action === 'link-account'
      ? {state: 'succeeded', output: {value: {code: 'ABCDEFGHJKLM'}}}
      : {state: 'failed', error: {code: 'unknown_action', message: 'no such action'}};
    return {done: Promise.resolve({terminal: true, update}), status: async () => ({terminal: true, update})};
  }};
  assert.deepEqual(await runRoomAction(connection, {id: 1, app: 'so.tilo.discord', action: 'link-account', arguments: {}}),
    {ok: {code: 'ABCDEFGHJKLM'}});
  assert.deepEqual(calls[0], {target: {provider: 'record:so.tilo.discord:', address: {resource: '', action: 'link-account'}}, argument: 'None'});
  assert.deepEqual(await runRoomAction(connection, {id: 2, app: 'so.tilo.chat', action: 'my-link', arguments: {}}),
    {error: 'no such action'});
  assert.deepEqual(await runRoomAction(undefined, {id: 3, app: 'so.tilo.discord', action: 'my-link'}),
    {error: 'this page reaches no room host'});
});

test('native requests preserve their correlation and exact query through the certified catalogue', async () => {
  const query = {provider:'record:so.tilo.chat:', view_resource:{id:'chat.message:fixture', kind:'chat.message'}};
  const replies = [], calls = [];
  let requests = [{CatalogueRequest:{correlation:42, query}}];
  const page = {rows:[], revision:'governed-read'};
  const connection = {catalogue:async request=>{calls.push(request); return page;}};
  const bridge = createNativeCatalogueBridge({current:()=>true, actions:()=>connection,
    take:()=>requests.splice(0), deliver:reply=>replies.push(reply)});
  bridge.pump();
  assert.deepEqual(calls, [], 'no reentrant runtime call during its status callback');
  await new Promise(done=>setImmediate(done));
  assert.deepEqual(calls, [query]);
  assert.deepEqual(replies, [{CatalogueResponse:{correlation:42, result:{Ok:page}}}]);
  bridge.dispose();
});

test('a closed mount cannot publish a late reply; a replaced connection refuses its old Read', async () => {
  for (const close of [true, false]) {
    let finish, connection = {catalogue:()=>new Promise(resolve=>{finish=resolve;})};
    const replies = [];
    let requests = [{CatalogueRequest:{correlation:7, query:{provider:'record:so.tilo.chat:'}}}];
    const bridge = createNativeCatalogueBridge({current:()=>true, actions:()=>connection,
      take:()=>requests.splice(0), deliver:reply=>replies.push(reply)});
    bridge.pump();
    await new Promise(done=>setImmediate(done));
    if (close) bridge.dispose(); else connection = {};
    finish({rows:[{resource:{id:'private'}}]});
    await new Promise(done=>setImmediate(done));
    if (close) assert.deepEqual(replies, []);
    else assert.ok(replies[0].CatalogueResponse.result.Err.Unavailable);
    bridge.dispose();
  }
});

test('runtime delivery failure is reported and releases the pending slot', async () => {
  const errors = [];
  let requests = [{CatalogueRequest:{correlation:9, query:{}}}];
  let delivered = 0;
  const bridge = createNativeCatalogueBridge({current:()=>true,
    actions:()=>connection, take:()=>requests.splice(0),
    deliver:()=>{delivered++; throw Error('closed runtime mount');},
    onError:error=>errors.push(error.message)});
  const connection = {catalogue:async()=>({rows:[]})};
  bridge.pump();
  await new Promise(done=>setImmediate(done));
  requests.push({CatalogueRequest:{correlation:9, query:{}}});
  bridge.pump();
  await new Promise(done=>setImmediate(done));
  assert.equal(delivered, 2, 'the same correlation is no longer held after failure');
  assert.deepEqual(errors, ['closed runtime mount', 'closed runtime mount']);
  bridge.dispose();
});

test('an app\'s ban becomes the room\'s membership request and answers with a receipt', async () => {
  const replies = [], calls = [];
  const invocation = '00000000-0000-4000-8000-000000000001';
  const target = action => ({provider:'tilo.workspace', address:{resource:'workspace.permissions:r', action}});
  let requests = [
    {Invoke:{invocation, target:target('ban'), argument:{Json:{person:'bb'.repeat(32)}}}},
    {Invoke:{invocation:'other', target:{provider:'record:so.tilo.chat:', address:{resource:'x', action:'ban'}}}},
  ];
  const rooms = {ban:async person=>{calls.push(person); return {};}};
  const bridge = createNativeCatalogueBridge({current:()=>true, actions:()=>undefined, membership:()=>rooms,
    take:()=>requests.splice(0), deliver:reply=>replies.push(reply)});
  bridge.pump();
  await new Promise(done=>setImmediate(done));
  assert.deepEqual(calls, ['bb'.repeat(32)]);
  assert.deepEqual(replies[0], {Receipt:{invocation:'other', status:{Rejected:{error:{Unavailable:{
    reason:'Only the room\'s membership actions run from an app here'}}}}}});
  assert.deepEqual(replies[1], {Receipt:{invocation, status:{Completed:{output:{}}}}});
  rooms.ban = async () => { throw Error('a Moderator cannot do this to a Moderator'); };
  requests.push({Invoke:{invocation, target:target('ban'), argument:{Json:{person:'cc'.repeat(32)}}}});
  bridge.pump();
  await new Promise(done=>setImmediate(done));
  assert.equal(replies[2].Receipt.status.Failed.error.message, 'a Moderator cannot do this to a Moderator');
  bridge.dispose();
});

test('an app\'s share-link asks the room\'s host as the person and answers with the link', async () => {
  const replies = [], asked = [];
  const invocation = '00000000-0000-4000-8000-000000000002';
  const target = {provider:'record:so.tilo.notes:', address:{resource:'notes.page:1', action:'share-link'}};
  const requests = [{Invoke:{invocation, target, argument:{Json:{start:1, end:2}}}}];
  const url = 'https://tilo.so/app/#/@h.example/w/r/so.tilo.notes/-/notes.page:1?chars=1-2';
  const connection = {invoke(wanted, argument, options) {
    asked.push({wanted, argument, options});
    return {done:Promise.resolve({terminal:true, update:{state:'succeeded', output:{value:{url}, resources:[]}}})};
  }};
  const bridge = createNativeCatalogueBridge({current:()=>true, actions:()=>connection,
    take:()=>requests.splice(0), deliver:reply=>replies.push(reply)});
  bridge.pump();
  await until(()=>replies.length, 'the share-link receipt');
  assert.deepEqual(asked, [{wanted:{provider:target.provider, address:target.address}, argument:{Json:{start:1, end:2}},
    options:{invocation}}]);
  assert.deepEqual(replies, [{Receipt:{invocation, status:{Completed:{output:{value:{url}, resources:[]}}}}}]);
  bridge.dispose();
});

test('the page removes someone through its own door: the room\'s membership request, on confirmation only', async () => {
  const calls = [];
  const room = 'r1';
  const person = 'bb'.repeat(32);
  const membership = {
    remove: async who => { calls.push(['remove', who]); return {room, members: [{userPublicKey: who, removed: true}]}; },
    unban: async who => { calls.push(['unban', who]); return {room, members: []}; },
  };
  const invoke = (action, args, options = {}) => invokeRoomMembership({membership, room,
    resource: roomGovernanceResource(room), action, args, ...options});
  // Not the room's membership: the application's door answers.
  assert.equal(invokeRoomMembership({membership, room, resource: 'notes.page:1', action: 'remove-member', args: {person}}), undefined);
  assert.equal(invokeRoomMembership({membership, room, resource: roomGovernanceResource('r2'), action: 'remove-member', args: {person}}),
    undefined, 'another room\'s governance');
  assert.equal(invoke('invite', {}), undefined, 'a verb the page does not take here');
  assert.equal(invoke('constructor', {person}), undefined, 'an inherited name is no verb');
  assert.equal(invokeRoomMembership({membership, room: undefined, resource: roomGovernanceResource(undefined),
    action: 'remove-member', args: {person}}), undefined, 'a peer in no room');
  // Removing someone asks first, and is the host's to decide.
  await assert.rejects(invoke('remove-member', {person}), /^Error: action_confirmation_required: /);
  await assert.rejects(invoke('remove-member', {person: 'u_guest'}, {confirm: true}), /^Error: invalid_arguments: /);
  await assert.rejects(invoke('remove-member', {person, reason: 'spam'}, {confirm: true}), /^Error: invalid_arguments: /);
  assert.deepEqual(calls, []);
  assert.deepEqual(await invoke('remove-member', {person}, {confirm: true}),
    {state: 'completed', output: {value: {room, members: [{userPublicKey: person, removed: true}]}}});
  // Letting someone back in takes nothing from anyone: no confirmation.
  assert.equal((await invoke('unban', {person})).state, 'completed');
  assert.deepEqual(calls, [['remove', person], ['unban', person]]);
  membership.remove = async () => { throw Error('a Moderator cannot do this to a Moderator'); };
  await assert.rejects(invoke('remove-member', {person}, {confirm: true}),
    {message: 'action_failed: a Moderator cannot do this to a Moderator'});
  await assert.rejects(invokeRoomMembership({membership: undefined, room, resource: roomGovernanceResource(room),
    action: 'remove-member', args: {person}, confirm: true}), /^Error: action_unavailable: /);
});
