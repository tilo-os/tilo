import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createHash, webcrypto} from 'node:crypto';
import {createDataLoader} from './data-loader.mjs';

const font=new Uint8Array([1,2,3,4]);
const manifest={schema:1,assets:[{name:'typst/Font.otf',bytes:font.byteLength,sha256:createHash('sha256').update(font).digest('hex')}]};

function fixture(files){
  const requests=[],installed=new Map(),failed=new Map(),fetched=[];
  const runtime={data_requests:()=>JSON.stringify(requests.splice(0)),install_data:(name,bytes)=>installed.set(name,bytes),data_failed:(name,reason)=>failed.set(name,reason)};
  const fetch=async url=>{
    const path=new URL(url).pathname.replace('/bundle/','');fetched.push(path);
    const body=files[path];
    return body===undefined?new Response('',{status:404}):new Response(typeof body==='string'?body:body);
  };
  const loader=createDataLoader({baseURL:'https://tilo.invalid/bundle/host.mjs',runtime,fetch,crypto:webcrypto});
  return {requests,installed,failed,fetched,loader};
}
/** The shared hang bound: a wait this long means the code under test is stuck. */
const HANG_BOUND_MS = Number(process.env.TILO_TEST_HANG_BOUND_SECS || 120) * 1000;
// Digest verification runs off the event loop: wait until every requested
// file was either installed or reported failed.
async function settled({installed,failed},count){
  const deadline=Date.now()+HANG_BOUND_MS;
  while(installed.size+failed.size<count){
    if(Date.now()>deadline)throw new Error('the loader never settled its requests');
    await new Promise(resolve=>setImmediate(resolve));
  }
}

test('requested data is fetched from data/, verified and installed',async()=>{
  const fx=fixture({'data.json':JSON.stringify(manifest),'data/typst/Font.otf':font});
  const {requests,installed,failed,fetched,loader}=fx;
  requests.push('typst/Font.otf');loader.pump();await settled(fx,1);
  assert.deepEqual([...installed.get('typst/Font.otf')],[...font]);
  assert.equal(failed.size,0);
  assert.deepEqual(fetched,['data.json','data/typst/Font.otf']);
});

test('unlisted names, mismatched bytes and a missing manifest fail instead of installing',async()=>{
  const bad=fixture({'data.json':JSON.stringify(manifest),'data/typst/Font.otf':new Uint8Array([9,9,9,9])});
  bad.requests.push('typst/Font.otf','typst/Other.otf');bad.loader.pump();await settled(bad,2);
  assert.equal(bad.installed.size,0);
  assert.match(bad.failed.get('typst/Font.otf'),/verification/);
  assert.match(bad.failed.get('typst/Other.otf'),/not in this bundle/);
  const none=fixture({});
  none.requests.push('typst/Font.otf');none.loader.pump();await settled(none,1);
  assert.match(none.failed.get('typst/Font.otf'),/HTTP 404/);
});

test('a failed file is reported and fetched, verified and installed again on retry',async()=>{
  const files={'data.json':JSON.stringify(manifest)};
  const fx=fixture(files);
  const {requests,installed,failed,loader}=fx;
  const reports=[];
  requests.push('typst/Font.otf');loader.pump(report=>reports.push(report));await settled(fx,1);
  assert.equal(installed.size,0);
  assert.deepEqual(reports.map(r=>[r.data,r.dataAsset]),[['unavailable','typst/Font.otf']]);
  await assert.rejects(loader.retry('typst/Font.otf',report=>reports.push(report)),/HTTP 404/);
  files['data/typst/Font.otf']=new Uint8Array([9,9,9,9]);
  await assert.rejects(loader.retry('typst/Font.otf'),/verification/);
  files['data/typst/Font.otf']=font;
  await loader.retry('typst/Font.otf',report=>reports.push(report));
  assert.deepEqual([...installed.get('typst/Font.otf')],[...font]);
  assert.deepEqual(reports.at(-1),{data:'available',dataAsset:'typst/Font.otf'});
  assert.ok(failed.has('typst/Font.otf'));
  await assert.rejects(loader.retry('typst/Font.otf'),/has not failed/);
});

test('a manifest that failed to load is fetched again by a retry',async()=>{
  const files={'data/typst/Font.otf':font};
  const fx=fixture(files);
  const {requests,installed,loader}=fx;
  requests.push('typst/Font.otf');loader.pump();await settled(fx,1);
  files['data.json']=JSON.stringify(manifest);
  await loader.retry('typst/Font.otf');
  assert.deepEqual([...installed.get('typst/Font.otf')],[...font]);
});
