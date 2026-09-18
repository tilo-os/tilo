import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createIdentityCustodian} from './identity-custody.mjs';
function fixture(){
 const held=new Set(),rows=new Map();let next=0,failSave=false,loads=0;
 const locks={async request(name,_opts,fn){if(held.has(name))return fn(null);held.add(name);try{return await fn({});}finally{held.delete(name);}}};
 const store={async load(key){assert(held.has(`qos-certified-device:${key}`));loads++;return rows.get(key);},async save(key,value){if(failSave)throw Error('identity commit aborted');rows.set(key,value);}};
 const runtime={create_identity:()=>({key:`device-${++next}`,scope:null}),identity_request:material=>JSON.stringify({device_public_key:material.key,request_id:material.key})};
 const session=()=>{const map=new Map();return{getItem:k=>map.get(k)??null,setItem:(k,v)=>map.set(k,v)}};
 const open=createIdentityCustodian({runtime,locks,store});return{open,session,held,rows,counts:()=>({next,loads}),failSave:()=>{failSave=true;}};
}
test('eight viewers have independent custody; duplicated locator never loads occupied secret',async()=>{
 const f=fixture(),session=f.session(),leases=[];for(let i=0;i<8;i++)leases.push(await f.open('/enrollment',session));
 assert.equal(new Set(leases.map(lease=>lease.publicKey)).size,8);assert.equal(f.held.size,8);assert.equal(f.counts().loads,0);
 await Promise.all(leases.map(lease=>lease.release()));assert.equal(f.held.size,0);
});
test('reload resumes exact material only after exclusive lock; commit failure releases custody',async()=>{
 const f=fixture(),session=f.session();let lease=await f.open('/enrollment',session);await lease.save({...lease.material,scope:'room'});const key=lease.publicKey;await lease.release();
 lease=await f.open('/enrollment',session);assert.equal(lease.publicKey,key);assert.equal(lease.material.scope,'room');assert.equal(f.counts().loads,1);await lease.release();
 f.failSave();await assert.rejects(f.open('/different',f.session()),/commit aborted/);assert.equal(f.held.size,0);
});
test('missing stored identity is explicit, never silently replaced; pre-aborted setup acquires nothing',async()=>{
 const f=fixture(),session=f.session();const lease=await f.open('/enrollment',session);await lease.release();f.rows.clear();
 await assert.rejects(f.open('/enrollment',session),/missing/);assert.equal(f.held.size,0);assert.equal(f.counts().next,1);
 const abort=new AbortController();abort.abort();await assert.rejects(f.open('/other',session,abort.signal));assert.equal(f.held.size,0);
});

test('twenty open/close cycles preserve identity, while separate profile stores remain independent',async()=>{
 const f=fixture(),session=f.session();let original;for(let i=0;i<20;i++){const lease=await f.open('/enrollment',session);original??=lease.publicKey;assert.equal(lease.publicKey,original);await lease.release();assert.equal(f.held.size,0);}assert.equal(f.counts().next,1);
 const other=await f.open('/enrollment',f.session());assert.notEqual(other.publicKey,original);await other.release();
});
