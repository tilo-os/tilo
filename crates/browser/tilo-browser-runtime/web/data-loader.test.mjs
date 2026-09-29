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
// Digest verification runs off the event loop, so give it several turns.
const settle=async()=>{for(let i=0;i<20;i++)await new Promise(resolve=>setTimeout(resolve,10));};

test('requested data is fetched from data/, verified and installed',async()=>{
  const {requests,installed,failed,fetched,loader}=fixture({'data.json':JSON.stringify(manifest),'data/typst/Font.otf':font});
  requests.push('typst/Font.otf');loader.pump();await settle();
  assert.deepEqual([...installed.get('typst/Font.otf')],[...font]);
  assert.equal(failed.size,0);
  assert.deepEqual(fetched,['data.json','data/typst/Font.otf']);
});

test('unlisted names, mismatched bytes and a missing manifest fail instead of installing',async()=>{
  const bad=fixture({'data.json':JSON.stringify(manifest),'data/typst/Font.otf':new Uint8Array([9,9,9,9])});
  bad.requests.push('typst/Font.otf','typst/Other.otf');bad.loader.pump();await settle();
  assert.equal(bad.installed.size,0);
  assert.match(bad.failed.get('typst/Font.otf'),/verification/);
  assert.match(bad.failed.get('typst/Other.otf'),/not in this bundle/);
  const none=fixture({});
  none.requests.push('typst/Font.otf');none.loader.pump();await settle();
  assert.match(none.failed.get('typst/Font.otf'),/HTTP 404/);
});

test('a failed file is reported and fetched, verified and installed again on retry',async()=>{
  const files={'data.json':JSON.stringify(manifest)};
  const {requests,installed,failed,loader}=fixture(files);
  const reports=[];
  requests.push('typst/Font.otf');loader.pump(report=>reports.push(report));await settle();
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
  const {requests,installed,loader}=fixture(files);
  requests.push('typst/Font.otf');loader.pump();await settle();
  files['data.json']=JSON.stringify(manifest);
  await loader.retry('typst/Font.otf');
  assert.deepEqual([...installed.get('typst/Font.otf')],[...font]);
});
