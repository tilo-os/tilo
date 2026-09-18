import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createIdentityStore} from './storage.mjs';
function database(){
 const rows=new Map();let current,closed=0;
 const db={close(){closed++;},transaction(_name,mode,options){const tx=new EventTarget();tx.error=null;const staged=new Map();tx.objectStore=()=>({put(value,key){staged.set(key,structuredClone(value));},get(key){const request={};queueMicrotask(()=>{request.result=rows.get(key);request.onsuccess();});return request;}});current={tx,mode,options,commit(){for(const [key,value]of staged)rows.set(key,value);tx.dispatchEvent(new Event('complete'));},abort(){tx.dispatchEvent(new Event('abort'));}};return tx;}};
 const idb={open(){const request={};queueMicrotask(()=>{request.result=db;request.onsuccess();});return request;}};
 return{store:createIdentityStore({idb}),rows,current:()=>current,closed:()=>closed};
}
const tick=()=>new Promise(resolve=>setImmediate(resolve));
test('identity storage acknowledges strict transaction commit, and aborted replacement retains original key',async()=>{
 const f=database();let acknowledged=false;const first=f.store.save('key',new Uint8Array([1,2,3])).then(()=>{acknowledged=true;});await tick();assert.equal(acknowledged,false);assert.deepEqual(f.current().options,{durability:'strict'});assert.equal(f.rows.size,0);f.current().commit();await first;assert.deepEqual(f.rows.get('key'),new Uint8Array([1,2,3]));
 const replacement=f.store.save('key',new Uint8Array([9]));await tick();f.current().abort();await assert.rejects(replacement,/aborted/);assert.deepEqual(f.rows.get('key'),new Uint8Array([1,2,3]));assert.equal(f.closed(),2);
 let loaded=false;const read=f.store.load('key').then(value=>{loaded=true;return value;});await tick();assert.equal(loaded,false);f.current().commit();assert.deepEqual(await read,new Uint8Array([1,2,3]));
});
