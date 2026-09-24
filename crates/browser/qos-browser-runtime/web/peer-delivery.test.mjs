import test from 'node:test';
import assert from 'node:assert/strict';
import {createPeerDelivery} from './peer-delivery.mjs';
const request=(id,target='/peer/v3/bootstrap',extra={})=>({kind:'request',id,method:'POST',origin:'https://host.test',target,body:[1],proof:null,...extra});
const body=(bytes=[7])=>new Response(Uint8Array.from(bytes),{status:200});
function setup(overrides={}) {
 const calls=[];const runtime={peer_delivery_begin(){calls.push('begin');return JSON.stringify(request(1));},peer_delivery_advance(_peer,id,bytes){calls.push(['advance',id,[...bytes]]);return JSON.stringify(id===1?{kind:'persist'}:{kind:'ready'});},peer_delivery_resume(){calls.push('resume');return JSON.stringify(request(2,'/peer/v3/challenge'));},peer_delivery_next(){return JSON.stringify({kind:'idle'});},peer_delivery_forget(_peer,id){calls.push(['forget',id]);},peer_delivery_stop(){calls.push('stop');},...overrides};
 return {calls,runtime};
}
test('delivery waits for actual persistence before HTTP proof stages and never opens another carrier',async()=>{
 const {calls,runtime}=setup();let release;const saved=new Promise(r=>release=r);const requests=[];
 const client=createPeerDelivery({runtime,peerId:4,persist:()=>{calls.push('persist');return saved;},fetch:async(url,options)=>{requests.push([url,options]);return body();}});
 const start=client.start();await new Promise(r=>setImmediate(r));assert.equal(requests.length,1);assert.deepEqual(calls.slice(0,3),['begin',['advance',1,[7]],['forget',1]]);assert.ok(calls.includes('persist'));assert.ok(!calls.includes('resume'));
 release();await start;assert.equal(requests.length,2);assert.equal(requests[0][1].credentials,'omit');assert.equal(requests[0][1].redirect,'error');assert.equal(requests[0][1].cache,'no-store');await client.close();
});
test('close joins a late raw fetch and fences its response before releasing custody',async()=>{
 const {calls,runtime}=setup();let resolve;let options;const pending=new Promise(r=>resolve=r);
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:(_url,value)=>{options=value;return pending;}});
 const start=client.start();start.catch(()=>{});await new Promise(r=>setImmediate(r));let settled=false;const close=client.close().then(()=>settled=true);assert.equal(options.signal.aborted,true);await new Promise(r=>setImmediate(r));assert.equal(settled,false);
 resolve(body());await close;await assert.rejects(start);assert.equal(calls.some(v=>Array.isArray(v)&&v[0]==='advance'),false);assert.equal(client.close(),client.close());
});
test('executor rejects aliased, foreign, unproved or oversized prepared data requests',async()=>{
 for(const prepared of [request(1,'/peer/v3/state?x=1',{proof:[1]}),request(1,'/peer/v3/state',{proof:null}),request(1,'/peer/v3/state',{origin:'https://host.test/extra',proof:[1]}),request(1,'/peer/v3/state',{proof:[1],body:Array(1048577).fill(1)})]) {
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(prepared)});let fetched=false;
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>{fetched=true;return body();}});await assert.rejects(client.start());assert.equal(fetched,false);await client.close();
 }
});
test('bounded streamed response rejection never advances Rust and cancels the reader',async()=>{
 const {calls,runtime}=setup();let cancelled=false;
 const stream=new ReadableStream({start(c){c.enqueue(new Uint8Array(32769));},cancel(){cancelled=true;}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>new Response(stream)});
 await assert.rejects(client.start(),/bound/);assert.equal(cancelled,true);assert.equal(calls.some(v=>Array.isArray(v)&&v[0]==='advance'),false);await client.close();
});
test('exact poll and ACK allow empty HTTP200 but malformed cursor and oversized private request fail closed',async()=>{
 for(const prepared of [request(1,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]}),request(1,'/peer/v3/acks',{proof:[1]})]){
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(prepared),peer_delivery_advance:()=>JSON.stringify({kind:'ready'})});
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>body([])});await client.start();await client.close();
 }
 for(const prepared of [request(1,'/peer/v3/events?cursor=abc&mode=poll',{method:'GET',body:[],proof:[1]}),request(1,'/peer/v3/reads',{body:Array(262157).fill(1),proof:[1]})]){
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(prepared),peer_delivery_advance:()=>JSON.stringify({kind:'ready'})});let fetched=false;
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>{fetched=true;return body();}});await assert.rejects(client.start());assert.equal(fetched,false);await client.close();
 }
});
test('verified transfer steps stream through one lease and attach cleanup only to the exact captured batch',async()=>{
 const transfer={id:'0123456789abcdef0123456789abcdef',scope:'scope',cutToken:'cut',totalBytes:3,maxItemBytes:3};const handle={...transfer,epoch:'lease',nextOffset:0};let opened=false,written=false,read=false,committed=false;
 const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'stage-open',transfer}),peer_delivery_stage_opened:(_p,id,offset)=>{assert.equal(id,transfer.id);assert.equal(offset,0);return JSON.stringify({kind:'stage-write',transferId:id,offset:0,bytes:[1,2,3]});},peer_delivery_stage_written:(_p,id,offset)=>{assert.equal(offset,3);return JSON.stringify({kind:'stage-read',transferId:id,offset:0,length:3});},peer_delivery_stage_read:(_p,id,offset,bytes)=>{assert.equal(id,transfer.id);assert.equal(offset,0);assert.deepEqual([...bytes],[1,2,3]);return JSON.stringify({kind:'persist'});},peer_delivery_resume:()=>JSON.stringify({kind:'ready'})});
 const transfers={async open(value){assert.deepEqual(value,transfer);opened=true;return handle;},async write(value,offset,bytes){assert.equal(value,handle);assert.equal(offset,0);assert.deepEqual([...bytes],[1,2,3]);written=true;},async status(value){assert.equal(value,handle);assert.ok(written);return {...handle,nextOffset:3};},async read(value,offset,length){assert.equal(value,handle);assert.equal(offset,0);assert.equal(length,3);read=true;return Uint8Array.of(1,2,3);}};
 let client;client=createPeerDelivery({runtime,peerId:4,transfers,persist:async()=>{assert.ok(read);await client.commitBatch([{revision:'7'}],{id:'cut-batch',transferId:transfer.id});},commit:async(writes,options)=>{assert.deepEqual(writes,[{revision:'7'}]);assert.equal(options.transfer,handle);committed=true;},fetch:()=>{throw Error('no network');}});
 await client.start();assert.ok(opened&&written&&read&&committed);await assert.rejects(client.commitBatch([],{transferId:transfer.id}),/transfer/);await client.close();
});
test('close joins pending transfer IO and rejects its late Rust continuation',async()=>{
 let resolve,continued=false;const id='0123456789abcdef0123456789abcdef';const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'stage-open',transfer:{id}}),peer_delivery_stage_opened:()=>{continued=true;return JSON.stringify({kind:'ready'});}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},transfers:{open:()=>new Promise(r=>resolve=r)}});const start=client.start();start.catch(()=>{});await new Promise(r=>setImmediate(r));let closed=false;const close=client.close().then(()=>closed=true);await new Promise(r=>setImmediate(r));assert.equal(closed,false);resolve({id,nextOffset:0});await close;await assert.rejects(start);assert.equal(continued,false);
});
test('shared authorization refresh coalesces and joins old raw work before fresh proofs on the same Peer',async()=>{
 let begun=0,scheduled=false,release,options;const requests=[];const {runtime}=setup({peer_delivery_begin:()=>{begun++;return JSON.stringify({kind:'ready'});},peer_delivery_next:()=>{if(scheduled)return JSON.stringify({kind:'idle'});scheduled=true;return JSON.stringify(request(5,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]}));}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async(_url,value)=>{options=value;requests.push(1);return new Promise(r=>release=r);}});
 await client.start();await new Promise(r=>setImmediate(r));const one=client.refreshAuthorization(),two=client.refreshAuthorization();assert.equal(one,two);assert.equal(options.signal.aborted,true);await new Promise(r=>setImmediate(r));assert.equal(begun,1);
 release(body([]));await one;assert.equal(begun,2);assert.equal(requests.length,1);await client.close();
});
test('close during shared authorization refresh never creates a new proof owner',async()=>{
 let begun=0,scheduled=false,release;const {runtime}=setup({peer_delivery_begin:()=>{begun++;return JSON.stringify({kind:'ready'});},peer_delivery_next:()=>{if(scheduled)return JSON.stringify({kind:'idle'});scheduled=true;return JSON.stringify(request(5,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]}));}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>new Promise(r=>release=r)});await client.start();await new Promise(r=>setImmediate(r));const refresh=client.refreshAuthorization();refresh.catch(()=>{});const close=client.close();release(body([]));await close;await assert.rejects(refresh);assert.equal(begun,1);
});
test('transport failure and stop do not block the existing canonical storage transaction',async()=>{
 let committed=0;const {runtime}=setup();const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},commit:async()=>committed++,fetch:async()=>{throw Error('offline');}});
 await assert.rejects(client.start(),/offline/);await client.commitBatch([{revision:'1'}],{id:'local'});await client.close();await client.commitBatch([{revision:'2'}],{id:'local2'});assert.equal(committed,2);
});
test('request deadline reports timeout promptly while retaining raw fetch custody until it settles',async()=>{
 const timers=new Map();let next=0,release,options,failure;const {runtime}=setup();
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock:{setTimeout(fn,ms){assert.equal(ms,10000);timers.set(++next,fn);return next;},clearTimeout(id){timers.delete(id);}},onError:error=>failure=error,fetch:async(_url,value)=>{options=value;return new Promise(r=>release=r);}});
 const start=client.start();start.catch(()=>{});await new Promise(r=>setImmediate(r));assert.equal(timers.size,1);[...timers.values()][0]();assert.match(failure.message,/timed out/);assert.equal(options.signal.aborted,true);
 let closed=false;const close=client.close().then(()=>closed=true);await new Promise(r=>setImmediate(r));assert.equal(closed,false);release(body());await close;await assert.rejects(start);assert.equal(timers.size,0);
});
test('transport close does not detach; explicit final detach joins its exact request once',async()=>{
 const {runtime,calls}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_detach:()=>JSON.stringify(request(9,'/peer/v3/sessions/'+'ab'.repeat(16),{method:'DELETE',body:[],proof:[1]})),peer_delivery_advance:()=>JSON.stringify({kind:'idle'})});
 let entered,release;const started=new Promise(r=>entered=r),gate=new Promise(r=>release=r);const requests=[];
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async(url,options)=>{requests.push([url,options.method]);entered();await gate;return body([]);}});
 await client.start();await client.close();assert.equal(requests.length,0);
 const pending=client.detach();await started;assert.equal(client.detach(),pending);let done=false;pending.then(()=>done=true);await new Promise(r=>setImmediate(r));assert.equal(done,false);
 release();await pending;assert.equal(requests.length,1);assert.equal(requests[0][1],'DELETE');assert.ok(calls.some(value=>Array.isArray(value)&&value[0]==='forget'&&value[1]===9));
});
test('renewed stage CAS keeps immutable cleanup handle and waits for its strict receipt',async()=>{
 const h={id:'01'.repeat(16),scope:'scope',cutToken:'old',resumeDigest:'old',totalBytes:1,maxItemBytes:1,nextOffset:1};
 let release,entered;const gate=new Promise(r=>release=r),started=new Promise(r=>entered=r);let acknowledged=false,cleaned,casEntered=false;
 const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'stage-open',transfer:h}),peer_delivery_stage_opened:()=>JSON.stringify({kind:'stage-renewed',transferId:h.id,expectedDigest:'old',resumeDigest:'new'}),peer_delivery_stage_renewed:(_peer,id,digest)=>{assert.equal(id,h.id);assert.equal(digest,'new');acknowledged=true;return JSON.stringify({kind:'ready'});}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},transfers:{open:async()=>h,compareAndSwapResumeDigest:async(handle,expected,next)=>{assert.equal(handle,h);assert.equal(expected,'old');assert.equal(next,'new');casEntered=true;entered();await gate;return {...h,resumeDigest:'new'};}},commit:async(_writes,options)=>cleaned=options.transfer});
 const pending=client.start();pending.catch(()=>{});await new Promise(resolve=>setImmediate(resolve));assert.equal(casEntered,true,'validated renewal must enter the durable CAS');assert.equal(acknowledged,false);release();await pending;
 await client.commitBatch([],{transferId:h.id});assert.equal(cleaned,h);assert.equal(cleaned.cutToken,'old');await client.close();
});
test('staging discovery stays inert and raw list custody is joined on close',async()=>{
 let release,entered,advanced=0;const gate=new Promise(r=>release=r),started=new Promise(r=>entered=r);
 const {runtime,calls}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'stage-list',id:12}),peer_delivery_stage_list:()=>{advanced++;return JSON.stringify({kind:'ready'});}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},transfers:{list:async()=>{entered();await gate;return [];}}});
 const pending=client.start();pending.catch(()=>{});await new Promise(r=>setImmediate(r));assert.ok(!calls.includes('stop'),'stage discovery must be a supported bounded transition');await started;
 let done=false;const closing=client.close().then(()=>done=true);await new Promise(r=>setImmediate(r));assert.equal(done,false);release();await closing;await assert.rejects(pending);assert.equal(advanced,0);assert.ok(calls.some(value=>Array.isArray(value)&&value[0]==='forget'&&value[1]===12));
});
test('an admitted cut is ACKed only after its storage transaction commits and Rust confirms it',async()=>{
 const order=[];let step=0;const poll=request(5,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]}),ack=request(6,'/peer/v3/acks',{proof:[1]});
 const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_next:()=>{
  // Like Rust: nothing but idle while a staged cut awaits its commit receipt.
  if(step===0){step=1;return JSON.stringify(poll);}if(step===2){step=3;return JSON.stringify(ack);}return JSON.stringify({kind:'idle'});},
 peer_delivery_advance:(_p,id)=>{order.push(['advance',id]);return JSON.stringify(id===5?{kind:'persist'}:{kind:'idle'});},peer_delivery_resume:()=>{order.push('resume');step=2;return JSON.stringify({kind:'idle'});}});
 let release;const committed=new Promise(r=>release=r);
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{order.push('persist');await committed;order.push('committed');},fetch:async url=>{order.push(new URL(url).pathname);return body([]);}});
 await client.start();for(let i=0;i<4;i++)await new Promise(r=>setImmediate(r));
 assert.deepEqual(order,['/peer/v3/events',['advance',5],'persist'],'no ACK while the transaction is pending');
 release();for(let i=0;i<6;i++)await new Promise(r=>setImmediate(r));
 assert.deepEqual(order,['/peer/v3/events',['advance',5],'persist','committed','resume','/peer/v3/acks',['advance',6]]);
 await client.close();
});
test('an aborted or quota-refused cut transaction never resumes, ACKs or polls past its cursor',async()=>{
 for(const name of ['QuotaExceededError','AbortError']){
  let step=0,failure;const fetched=[];const poll=request(5,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]});
  const {runtime,calls}=setup({peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_next:()=>{step++;return JSON.stringify(step===1?poll:{kind:'idle'});},peer_delivery_advance:()=>JSON.stringify({kind:'persist'})});
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{throw Object.assign(Error('transaction failed'),{name});},onError:error=>failure=error,fetch:async url=>{fetched.push(new URL(url).pathname);return body([]);}});
  await client.start();for(let i=0;i<6;i++)await new Promise(r=>setImmediate(r));
  assert.equal(failure?.name,name);assert.ok(!calls.includes('resume'),'Rust never observes an unconfirmed commit');
  assert.ok(calls.includes('stop'),'the delivery session stops; pending writes remain for storage retry');
  assert.deepEqual(fetched,['/peer/v3/events']);await client.close();
 }
});

test('refused data stream discards only its HTTP session before reporting a terminal failure',async()=>{
 for(const [target,status,expected] of [['/peer/v3/events?mode=poll',403,1],['/peer/v3/acks',401,1],['/peer/v3/commands/invoke',403,0],['/peer/v3/events?mode=poll',503,0]]){
  let discarded=0,errors=[];
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(request(1,target,{method:target.startsWith('/peer/v3/events')?'GET':'POST',body:target.startsWith('/peer/v3/events')?[]:[1],proof:[1]})),peer_delivery_discard_session:()=>{discarded++;}});
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>new Response(new Uint8Array([1]),{status}),onError:error=>errors.push(error)});
  await assert.rejects(client.start(),error=>error.terminal===(status===401||status===403));
  assert.equal(discarded,expected,`${target} ${status}`);await client.close();
 }
});
test('a refused data stream renews the HTTP session in place once, then escalates within the window',async()=>{
 let begins=0,polls=0,errors=[];const poll=request(9,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]});
 const runtime={peer_delivery_begin(){begins++;polls=0;return JSON.stringify({kind:'ready'});},peer_delivery_advance:()=>JSON.stringify({kind:'idle'}),peer_delivery_next(){return JSON.stringify(polls++<1?poll:{kind:'idle'});},peer_delivery_forget(){},peer_delivery_stop(){},peer_delivery_discard_session(){}};
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock:{now:()=>1000},fetch:async()=>new Response('',{status:403}),onError:error=>errors.push(error)});
 await client.start();for(let i=0;i<20;i++)await new Promise(r=>setImmediate(r));
 assert.equal(begins,2,'first refusal renews the session without transport recovery');
 assert.equal(errors.length,1,'a second refusal inside the window escalates');assert.equal(errors[0].sessionRefused,true);await client.close();
});
test('a transient poll failure on an established session is retried in place without transport recovery',async()=>{
 let polls=0,errors=[],fetches=0;const poll=request(9,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]});
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_advance:()=>JSON.stringify({kind:'idle'}),peer_delivery_next(){return JSON.stringify(polls++<3?poll:{kind:'idle'});},peer_delivery_forget(){},peer_delivery_stop(){},peer_delivery_discard_session(){}};
 const timers=[];const clock={now:()=>0,setTimeout:(fn,ms)=>{timers.push(fn);return timers.length;},clearTimeout(){}};
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock,fetch:async()=>{fetches++;if(fetches<3)throw TypeError('Failed to fetch');return body([]);},onError:error=>errors.push(error)});
 await client.start();
 for(let round=0;round<6;round++){for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));const due=timers.splice(0);for(const fn of due)fn();}
 assert.equal(errors.length,0,'transient failures do not tear down the client');assert.ok(fetches>=3,'the poll was re-issued');await client.close();
});
test('a lost state upload reply is re-sent in place (the host answers with its durable receipt)',async()=>{
 let states=0,errors=[],fetches=0,forgotten=[];const state=id=>request(id,'/peer/v3/state',{proof:[1]});
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_advance:()=>JSON.stringify({kind:'idle'}),peer_delivery_next(){return JSON.stringify(states<2?state(10+states++):{kind:'idle'});},peer_delivery_forget(_p,id){forgotten.push(id);},peer_delivery_stop(){},peer_delivery_discard_session(){}};
 const timers=[];const clock={now:()=>0,setTimeout:(fn)=>{timers.push(fn);return timers.length;},clearTimeout(){}};
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock,fetch:async()=>{fetches++;if(fetches===1)throw TypeError('Failed to fetch');return body([]);},onError:error=>errors.push(error)});
 await client.start();
 for(let round=0;round<4;round++){for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));const due=timers.splice(0);for(const fn of due)fn();}
 assert.equal(errors.length,0,'a transient upload failure does not tear down delivery');
 assert.deepEqual(forgotten.slice(0,2),[10,11],'the failed handle is forgotten so Rust releases and re-plans its items');
 assert.ok(fetches>=2);await client.close();
});
test('a persisting cut lets the scheduler issue the next poll before the commit settles',async()=>{
 const order=[];let step=0,release;const committed=new Promise(r=>release=r);
 const poll=id=>request(id,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]});
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),
  peer_delivery_next(){
  // Like Rust: a second poll exists only once the first cut is persisting.
  if(step===0){step=1;return JSON.stringify(poll(21));}if(step===1&&order.includes('persist')){step=2;return JSON.stringify(poll(22));}return JSON.stringify({kind:'idle'});},
  peer_delivery_advance:(_p,id)=>{order.push(['advance',id]);return JSON.stringify(id===21?{kind:'persist'}:{kind:'idle'});},
  peer_delivery_resume:()=>{order.push('resume');return JSON.stringify({kind:'idle'});},peer_delivery_forget(){},peer_delivery_stop(){}};
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{order.push('persist');await committed;order.push('committed');},fetch:async()=>{order.push('fetch');return body([]);}});
 await client.start();for(let i=0;i<8;i++)await new Promise(r=>setImmediate(r));
 assert.deepEqual(order.slice(0,5),['fetch',['advance',21],'persist','fetch',['advance',22]],'second poll while the first cut persists');
 release();for(let i=0;i<4;i++)await new Promise(r=>setImmediate(r));assert.ok(order.includes('resume'));await client.close();
});
test('setup persistence never polls the scheduler before delivery custody exists',async()=>{
 let ready=false;const {runtime}=setup({peer_delivery_resume:()=>{ready=true;return JSON.stringify({kind:'ready'});},peer_delivery_next(){if(!ready)throw Error('delivery custody not installed');return JSON.stringify({kind:'idle'});}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{await new Promise(r=>setImmediate(r));},fetch:async()=>body()});
 await client.start();await client.close();
});
// PIPELINED_POLL_V1: chained held polls use the canonical sorted query.
const chained=(id,index,cursor='ab')=>request(id,`/peer/v3/events?chain=0123456789abcdef&cursor=${cursor}&index=${index}&mode=chain`,{method:'GET',body:[],proof:[1]});
test('chained polls are exact GET targets; malformed chains fail closed before fetch',async()=>{
 for(const prepared of [chained(1,0),chained(1,4294967295)]){
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(prepared),peer_delivery_advance:()=>JSON.stringify({kind:'ready'})});const fetched=[];
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async(url,options)=>{fetched.push([url,options.method,options.body]);return body([]);}});await client.start();
  assert.equal(fetched.length,1);assert.equal(fetched[0][1],'GET');assert.equal(fetched[0][2],undefined);await client.close();
 }
 for(const target of ['/peer/v3/events?chain=0123456789abcdef&index=1&mode=chain','/peer/v3/events?cursor=ab&mode=chain&chain=0123456789abcdef&index=1','/peer/v3/events?chain=0123456789abcdef&cursor=ab&index=01&mode=chain','/peer/v3/events?chain=0123456789ABCDEF&cursor=ab&index=1&mode=chain']){
  const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(request(1,target,{method:'GET',body:[],proof:[1]}))});let fetched=false;
  const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>{fetched=true;return body();}});await assert.rejects(client.start());assert.equal(fetched,false,target);await client.close();
 }
});
test('a refused chained poll discards its HTTP session like a plain poll',async()=>{
 let discarded=0;
 const {runtime}=setup({peer_delivery_begin:()=>JSON.stringify(chained(1,2)),peer_delivery_discard_session:()=>{discarded++;}});
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},fetch:async()=>new Response(new Uint8Array([1]),{status:403})});
 await assert.rejects(client.start(),error=>error.terminal===true&&error.sessionRefused===true);assert.equal(discarded,1);await client.close();
});
test('several chained polls stay outstanding together and a lost one is forgotten so Rust abandons the chain',async()=>{
 let issued=0,errors=[];const forgotten=[];const pending=[];
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_advance:()=>JSON.stringify({kind:'idle'}),peer_delivery_next(){return JSON.stringify(issued<3?chained(20+issued,issued++):{kind:'idle'});},peer_delivery_forget(_p,id){forgotten.push(id);},peer_delivery_stop(){},peer_delivery_discard_session(){}};
 const timers=[];const clock={now:()=>0,setTimeout:(fn)=>{timers.push(fn);return timers.length;},clearTimeout(){}};
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock,fetch:()=>new Promise((resolve,reject)=>pending.push({resolve,reject})),onError:error=>errors.push(error)});
 await client.start();for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));
 assert.equal(pending.length,3,'the whole chain is in flight at once');
 pending[1].reject(TypeError('Failed to fetch'));pending[0].resolve(body([]));pending[2].resolve(body([]));
 for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));
 assert.deepEqual([...forgotten].sort(),[20,21,22],'every handle is forgotten; the failed one without an advance');
 assert.equal(errors.length,0,'a lost chained poll is retried in place');await client.close();
});
test('a stale chained poll (409) restarts the chain at once without counting as a transport failure',async()=>{
 let issued=0,errors=[];const forgotten=[];let fetches=0;
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:'ready'}),peer_delivery_advance:()=>JSON.stringify({kind:'idle'}),peer_delivery_next(){return JSON.stringify(issued<6?chained(30+issued,issued++):{kind:'idle'});},peer_delivery_forget(_p,id){forgotten.push(id);},peer_delivery_stop(){},peer_delivery_discard_session(){}};
 const timers=[];const clock={now:()=>0,setTimeout:(fn)=>{timers.push(fn);return timers.length;},clearTimeout(){}};
 // Five stale answers in a row would exhaust the transient retry budget (3).
 const client=createPeerDelivery({runtime,peerId:4,persist:async()=>{},clock,fetch:async()=>{fetches++;return fetches<=5?new Response('stale-poll-chain',{status:409}):body([]);},onError:error=>errors.push(error)});
 await client.start();for(let i=0;i<30;i++)await new Promise(r=>setImmediate(r));
 assert.equal(errors.length,0,'stale chain answers never escalate to transport recovery');
 assert.equal(fetches,6,'each stale answer is followed at once, without backoff timers');
 assert.equal(forgotten.length,6);
 // A 409 on anything else is still an ordinary failure.
 const plain=setup({peer_delivery_begin:()=>JSON.stringify(request(1,'/peer/v3/events?mode=poll',{method:'GET',body:[],proof:[1]}))});
 const other=createPeerDelivery({runtime:plain.runtime,peerId:4,persist:async()=>{},fetch:async()=>new Response('x',{status:409})});
 await assert.rejects(other.start(),error=>error.staleChain===false);await other.close();await client.close();
});
