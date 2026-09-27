import assert from 'node:assert/strict';
import test from 'node:test';
import {MAX_OPEN_FILE_BYTES,checkOpens,loadOpenBytes,openRequest} from './open-file.mjs';

const base='https://tilo.so/demo/';
const books={id:'so.tilo.ebook',title:'Books',opens:['application/epub+zip']};

test('a URL names the file and its extension its media type', () => {
  const request=openRequest({url:'books/homer_the-odyssey.epub?source=download'},base);
  assert.equal(request.url.href,'https://tilo.so/demo/books/homer_the-odyssey.epub?source=download');
  assert.equal(request.name,'homer_the-odyssey.epub');
  assert.equal(request.mediaType,'application/epub+zip');
  assert.equal(request.bytes,undefined);
  assert.equal(openRequest({url:'/The%20Odyssey.EPUB'},base).name,'The Odyssey.EPUB');
  assert.equal(openRequest({url:'/The%20Odyssey.EPUB'},base).mediaType,'application/epub+zip');
});

test('explicit names and media types win and are normalised', () => {
  const request=openRequest({bytes:new ArrayBuffer(3),name:' odyssey ',mediaType:'Application/EPUB+zip; q=1'},base);
  assert.equal(request.name,'odyssey');
  assert.equal(request.mediaType,'application/epub+zip');
  assert.deepEqual(request.bytes,new Uint8Array(3));
});

test('malformed options are refused as invalid', () => {
  for(const open of [undefined,'book.epub',{},{url:'a.epub',bytes:new Uint8Array()},{bytes:new Uint8Array()},{bytes:[1],name:'a.epub'},{url:'/notes.txt'},{url:'/a.epub',mediaType:'epub'},{url:'http://[bad'}]){
    assert.throws(()=>openRequest(open,base),error=>error.code==='TILO_OPEN_INVALID',JSON.stringify(open));
  }
});

test('an app that does not open the type refuses it before any fetch', () => {
  checkOpens(books,'application/epub+zip');
  assert.throws(()=>checkOpens(books,'application/pdf'),{code:'TILO_OPEN_UNSUPPORTED',message:'Books does not open application/pdf files (it opens application/epub+zip)'});
  assert.throws(()=>checkOpens({title:'Chat',opens:[]},'application/epub+zip'),{code:'TILO_OPEN_UNSUPPORTED',message:'Chat does not open files'});
  assert.throws(()=>checkOpens(undefined,'application/epub+zip'),{code:'TILO_OPEN_UNSUPPORTED'});
});

test('bytes come from the admitted loader, bounded', async () => {
  const seen=[];
  const bytes=await loadOpenBytes(openRequest({url:'/o.epub'},base),async url=>{seen.push(url);return new Uint8Array([1,2]);});
  assert.deepEqual([...bytes],[1,2]);
  assert.deepEqual(seen,['https://tilo.so/o.epub']);
  const own=new Uint8Array([7]);
  assert.equal(await loadOpenBytes(openRequest({bytes:own,name:'a.epub'},base),()=>assert.fail('no load')),own);
  await assert.rejects(loadOpenBytes(openRequest({url:'/o.epub'},base),async()=>{throw Error('asset HTTP 404');}),{code:'TILO_OPEN_UNAVAILABLE',message:'Could not load o.epub: asset HTTP 404'});
  await assert.rejects(loadOpenBytes(openRequest({url:'/o.epub'},base),async()=>'text'),{code:'TILO_OPEN_UNAVAILABLE'});
  const huge={byteLength:MAX_OPEN_FILE_BYTES+1};
  Object.setPrototypeOf(huge,Uint8Array.prototype);
  await assert.rejects(loadOpenBytes({name:'huge.epub',bytes:huge},()=>{}),{code:'TILO_OPEN_INVALID'});
});
