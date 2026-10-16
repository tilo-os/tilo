import test from 'node:test';
import assert from 'node:assert/strict';
import {MAX_PICK_BYTES,openUri,pickFiles,saveFile} from './dialogs.mjs';

// A document with just enough DOM for the file input and download link.
function page(){
  const appended=[];
  const element=tag=>{
    const node=new EventTarget();
    Object.assign(node,{tag,style:{},clicks:0,removed:false,click(){node.clicks++;node.onclick?.();},remove(){node.removed=true;}});
    return node;
  };
  const document={createElement:element,body:{append:node=>appended.push(node)}};
  return {document,appended};
}
const file=(name,text,type='text/plain')=>Object.assign(new Blob([text],{type}),{name});

test('a file chooser delivers every chosen file as bytes',async()=>{
  const {document,appended}=page();
  const picked=pickFiles('.md,text/plain',true,null,{document});
  const [input]=appended;
  assert.equal(input.type,'file');assert.equal(input.accept,'.md,text/plain');assert.equal(input.multiple,true);assert.equal(input.clicks,1);
  input.files=[file('a.md','alpha'),file('b.bin','beta','')];
  input.dispatchEvent(new Event('change'));
  const files=await picked;
  assert.deepEqual(files.map(f=>[f.name,f.type,new TextDecoder().decode(f.bytes)]),[['a.md','text/plain','alpha'],['b.bin',null,'beta']]);
  assert.equal(input.removed,true,'the input leaves the page');
});

test('cancelling or choosing nothing resolves null; oversized picks reject',async()=>{
  for(const finish of [input=>input.dispatchEvent(new Event('cancel')),input=>{input.files=[];input.dispatchEvent(new Event('change'));}]){
    const {document,appended}=page();
    const picked=pickFiles('',false,null,{document});
    assert.equal(appended[0].accept,undefined,'no accept attribute admits any file');
    finish(appended[0]);
    assert.equal(await picked,null);
  }
  const {document,appended}=page();
  const picked=pickFiles('',false,null,{document});
  appended[0].files=[{...file('huge','x'),size:MAX_PICK_BYTES+1}];
  appended[0].dispatchEvent(new Event('change'));
  await assert.rejects(picked,/too large/);
});

test('a refused click rejects instead of hanging',async()=>{
  const {document,appended}=page();
  const original=document.createElement;
  document.createElement=tag=>Object.assign(original(tag),{click(){throw Error('not allowed');}});
  await assert.rejects(pickFiles('',false,null,{document}),/not allowed/);
  assert.equal(appended[0].removed,true);
});

test('saving downloads through a temporary object URL',()=>{
  const {document,appended}=page();const revoked=[],timers=[];
  const urls={createObjectURL:blob=>{assert.equal(blob.size,3);return 'blob:x';},revokeObjectURL:url=>revoked.push(url)};
  saveFile('report.txt',new Uint8Array([1,2,3]),{document,URL:urls,Blob,setTimeout:fn=>timers.push(fn)});
  const [link]=appended;
  assert.deepEqual([link.href,link.download,link.clicks,link.removed],['blob:x','report.txt',1,true]);
  timers.forEach(fn=>fn());assert.deepEqual(revoked,['blob:x']);
});

test('opened URIs lose their opener; a blocked pop-up reports false',()=>{
  const child={opener:'console'};
  assert.equal(openUri('https://tilo.so',{window:{open:(uri,target)=>{assert.deepEqual([uri,target],['https://tilo.so','_blank']);return child;}}}),true);
  assert.equal(child.opener,null);
  assert.equal(openUri('https://tilo.so',{window:{open:()=>null}}),false);
});

test('requested contents limits count every file and accept the exact budget',async()=>{
  for(const [limit,accepted] of [[4,true],[3,false]]){
    const {document,appended}=page();
    const picked=pickFiles('',true,limit,{document});
    appended[0].files=[file('a','ab'),file('b','cd')];
    appended[0].dispatchEvent(new Event('change'));
    if(accepted)assert.deepEqual((await picked).map(f=>f.bytes.length),[2,2]);
    else await assert.rejects(picked,/too large/);
  }
  const {document,appended}=page();
  const picked=pickFiles('',false,0,{document});
  appended[0].files=[file('empty','')];
  appended[0].dispatchEvent(new Event('change'));
  assert.equal((await picked)[0].bytes.length,0);
});

test('bounded browser reads slice before allocating, then reject excess actual bytes',async()=>{
  const {document,appended}=page();
  const picked=pickFiles('',true,3,{document});
  const slices=[];
  appended[0].files=[file('first','ab'),{
    name:'misreported',size:1,type:'',
    arrayBuffer(){throw Error('unbounded read');},
    slice(start,end){slices.push([start,end]);return new Blob(['xx']).slice(start,end);}
  }];
  appended[0].dispatchEvent(new Event('change'));
  await assert.rejects(picked,/too large/);
  assert.deepEqual(slices,[[0,2]],'only the remaining budget plus one byte is materialized');
  assert.equal(appended[0].removed,true);
});

test('oversized files are rejected before reading and invalid bounds refuse the chooser',async()=>{
  const {document,appended}=page();
  const picked=pickFiles('',false,2,{document});
  appended[0].files=[{name:'huge',size:3,slice(){throw Error('must not read oversized file');}}];
  appended[0].dispatchEvent(new Event('change'));
  await assert.rejects(picked,/too large/);
  for(const limit of [-1,NaN,Infinity,0.5]){
    const pageState=page();
    await assert.rejects(pickFiles('',false,limit,pageState),/Invalid contents byte limit/);
    assert.equal(pageState.appended.length,0);
  }
});
