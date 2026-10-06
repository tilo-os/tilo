import test from 'node:test';
import assert from 'node:assert/strict';
import {createNativeCatalogueBridge} from './native-catalogue.mjs';

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
