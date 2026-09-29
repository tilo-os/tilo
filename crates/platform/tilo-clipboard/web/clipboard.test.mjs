import test from 'node:test';
import assert from 'node:assert/strict';
import {writeClipboardText} from './clipboard.mjs';

// The external browser command/clipboard boundary is unavailable in Node.
function browser({allowed=true,dispatch=true,throws=false,transferFails=false}={}) {
  const document=new EventTarget();let text='previous',forwarded=0;
  document.execCommand=command=>{
    assert.equal(command,'copy');
    if(throws)throw Error('Clipboard blocked');
    if(!dispatch)return allowed;
    const event=new Event('copy',{cancelable:true});
    event.clipboardData={setData(type,value){assert.equal(type,'text/plain');if(transferFails)throw Error('Transfer refused');text=value;}};
    document.dispatchEvent(event);
    if(!event.defaultPrevented)forwarded++;
    return allowed;
  };
  return {document,text:()=>text,forwarded:()=>forwarded};
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
    assert.ok(reports[0].clipboardError);
    const event=new Event('copy',{cancelable:true});host.document.dispatchEvent(event);
    assert.equal(event.defaultPrevented,false,'failed commands also remove their handler');
  }
});

test('unavailable native copy reports failure',()=>{
  const reports=[];
  assert.equal(writeClipboardText('text',(ok,error)=>reports.push(ok?{clipboard:'copied'}:{clipboard:'failed',clipboardError:error}),null),false);
  assert.equal(reports[0].clipboard,'failed');
});
