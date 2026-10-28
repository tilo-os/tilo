import test from 'node:test';
import assert from 'node:assert/strict';
import {COPY_REFUSED,COPY_UNAVAILABLE,reserveClipboardText,writeClipboardRich,writeClipboardText} from './clipboard.mjs';

// The external browser command/clipboard boundary is unavailable in Node.
function browser({allowed=true,dispatch=true,throws=false,transferFails=false}={}) {
  const document=new EventTarget();let text='previous',forwarded=0;const data={};
  document.execCommand=command=>{
    assert.equal(command,'copy');
    if(throws)throw Error('Clipboard blocked');
    if(!dispatch)return allowed;
    const event=new Event('copy',{cancelable:true});
    event.clipboardData={setData(type,value){assert.ok(type==='text/plain'||type==='text/html');if(transferFails)throw Error('Transfer refused');data[type]=value;if(type==='text/plain')text=value;}};
    document.dispatchEvent(event);
    if(!event.defaultPrevented)forwarded++;
    return allowed;
  };
  return {document,text:()=>text,data:()=>data,forwarded:()=>forwarded};
}

test('clicked Copy synchronously publishes Unicode to the system transfer',()=>{
  const host=browser(),reports=[];
  assert.equal(writeClipboardText('Outside the shell — café 👋',(ok,error)=>reports.push(ok?{clipboard:'copied'}:{clipboard:'failed',clipboardError:error}),host.document),true);
  assert.equal(host.text(),'Outside the shell — café 👋');
  assert.deepEqual(reports,[{clipboard:'copied'}]);
  assert.equal(host.forwarded(),0,'native copy must not reenter the app keyboard handler');
  host.document.execCommand('copy');
  assert.equal(host.forwarded(),1,'temporary interception is removed after publication');
});

test('refused, missing, or failing transfers never authorize Cut deletion',()=>{
  for(const options of [{allowed:false},{dispatch:false},{throws:true},{transferFails:true}]){
    const host=browser(options),reports=[];
    assert.equal(writeClipboardText('text',(ok,error)=>reports.push(ok?{clipboard:'copied'}:{clipboard:'failed',clipboardError:error}),host.document),false);
    assert.equal(reports.length,1);
    assert.equal(reports[0].clipboard,'failed');
    // The page says the reason as it is: the person's next step, never the
    // browser's error text with its "Error:" in front.
    assert.equal(reports[0].clipboardError,COPY_REFUSED,JSON.stringify(options));
    const event=new Event('copy',{cancelable:true});host.document.dispatchEvent(event);
    assert.equal(event.defaultPrevented,false,'failed commands also remove their handler');
  }
});

test('unavailable native copy reports failure',()=>{
  const reports=[];
  assert.equal(writeClipboardText('text',(ok,error)=>reports.push(ok?{clipboard:'copied'}:{clipboard:'failed',clipboardError:error}),null),false);
  assert.equal(reports[0].clipboard,'failed');
  assert.equal(reports[0].clipboardError,COPY_UNAVAILABLE);
});

test('a rich copy publishes its HTML and its words in one transfer',()=>{
  const host=browser(),reports=[];
  assert.equal(writeClipboardRich('<p>plain <b>bold</b></p>','plain bold',(ok)=>reports.push(ok),host.document),true);
  assert.deepEqual(host.data(),{'text/html':'<p>plain <b>bold</b></p>','text/plain':'plain bold'});
  assert.deepEqual(reports,[true]);
});

// The async clipboard: `write` takes the promised items inside the click and
// settles once they resolve, as a browser does.
function asyncClipboard({refuse=false}={}) {
  const written=[];
  class Item { constructor(items){this.items=items;} }
  const navigator={clipboard:{async write(items){
    if(refuse)throw Error('Write permission denied');
    for(const item of items)written.push(await (await item.items['text/plain']).text());
  }}};
  return {navigator,Item,written};
}

test('a copy claimed in the click is written once its text arrives',async()=>{
  const browser=asyncClipboard(),reports=[];
  const fill=reserveClipboardText((ok,error)=>reports.push([ok,error]),browser.navigator,browser.Item);
  assert.equal(typeof fill,'function');
  await new Promise(done=>setTimeout(done,10));
  assert.deepEqual(reports,[],'nothing settles before the text is in');
  fill('https://tilo.so/app/#/@host/room/so.tilo.chat');
  await new Promise(done=>setTimeout(done,10));
  assert.deepEqual(browser.written,['https://tilo.so/app/#/@host/room/so.tilo.chat']);
  assert.deepEqual(reports,[[true,undefined]]);
});

test('a claimed copy given up copies nothing and reports no refusal',async()=>{
  const browser=asyncClipboard(),reports=[];
  const fill=reserveClipboardText((ok,error)=>reports.push([ok,error]),browser.navigator,browser.Item);
  fill(null);
  await new Promise(done=>setTimeout(done,10));
  assert.deepEqual(browser.written,[]);
  assert.deepEqual(reports,[[false,undefined]]);
});

test('a refused claimed copy says what to do, and no async clipboard claims nothing',async()=>{
  const browser=asyncClipboard({refuse:true}),reports=[];
  const fill=reserveClipboardText((ok,error)=>reports.push([ok,error]),browser.navigator,browser.Item);
  fill('link');
  await new Promise(done=>setTimeout(done,10));
  assert.deepEqual(reports,[[false,COPY_REFUSED]]);
  assert.equal(reserveClipboardText(()=>{},{},browser.Item),null);
  assert.equal(reserveClipboardText(()=>{},browser.navigator,undefined),null);
});
