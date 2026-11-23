import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source = fs.readFileSync(new URL('../src/credential_forms.js', import.meta.url), 'utf8');
function fixture() {
  const location={href:'https://example.com/login',origin:'https://example.com',protocol:'https:'};
  let document;
  class Input {
    constructor(type, autocomplete, value='') {
      Object.assign(this,{type,autocomplete,_value:value,disabled:false,readOnly:false,
        isConnected:true,hidden:false,events:[],ownerDocument:null,form:null});
    }
    get value(){return this._value;}
    set value(value){this._value=String(value);}
    getClientRects(){return this.hidden?[]:[{width:100,height:20}];}
    getRootNode(){return document;}
    dispatchEvent(event){this.events.push(event.type);return true;}
  }
  const form={action:location.href,isConnected:true,elements:[],ownerDocument:null};
  document={forms:[form]};form.ownerDocument=document;
  const add=(type,token,value='')=>{
    const input=new Input(type,token,value);input.ownerDocument=document;input.form=form;
    form.elements.push(input);return input;
  };
  const username=add('text','username','synthetic-user');
  const password=add('password','current-password','synthetic-secret');
  const context={document,location,URL,HTMLInputElement:Input,Event:class {constructor(type){this.type=type;}},
    getComputedStyle:()=>({display:'block',visibility:'visible',opacity:'1'})};
  context.window=context;context.top=context;
  const bridge=vm.runInNewContext(source,context);
  return {bridge,form,username,password,add};
}
{
  const f=fixture();const forms=f.bridge.discover();
  assert.equal(forms.length,1);assert(!JSON.stringify(forms).includes('synthetic-secret'));
  assert.equal(f.bridge.validate(forms[0].id),true);
  const captured=f.bridge.capture(forms[0].id);
  assert.equal(captured.username,'synthetic-user');assert.equal(captured.password,'synthetic-secret');
  const secret='quotes " \\ unicode π </script> literal';
  assert(f.bridge.fill(forms[0].id,'chosen-user',secret));
  assert.equal(f.password.value,secret);assert.equal(f.username.value,'chosen-user');
  assert.deepEqual(f.password.events,['input','change']);
}
for(const mutation of [
  f=>{f.password.isConnected=false;f.add('password','current-password');},
  f=>{f.password.hidden=true;},
  f=>{f.password.readOnly=true;},
  f=>{f.password.type='text';},
  f=>{f.form.action='https://other.example/steal';},
  f=>{f.form.action='http://example.com/insecure';},
]) {
  const f=fixture();const id=f.bridge.discover()[0].id;mutation(f);
  assert.equal(f.bridge.validate(id),false);
  assert.equal(f.bridge.capture(id),null);assert.equal(f.bridge.fill(id,'new','new'),false);
  assert.equal(f.password.value,'synthetic-secret');
}
{
  const f=fixture();const id=f.bridge.discover()[0].id;
  f.bridge.discover();assert.equal(f.bridge.capture(id),null);
}
{
  const f=fixture();const second=f.add('password','new-password','new-secret');
  const choices=f.bridge.discover();assert.equal(choices.length,2);
  assert(f.bridge.fill(choices[0].id,'user','old-account'));
  assert.equal(second.value,'new-secret'); // explicit selected field only
}
{
  const f=fixture();const address=f.add('text','street-address');
  const ids=f.bridge.discoverAddresses();assert.equal(ids.length,1);
  assert(f.bridge.fillAddress(ids[0].id,{street:'123 Synthetic Street'}));
  assert.equal(address.value,'123 Synthetic Street');assert.equal(f.password.value,'synthetic-secret');
  assert.equal(f.bridge.fillAddress(ids[0].id,{password:'forbidden'}),false);
}
{
  const f=fixture();const id=f.bridge.discover()[0].id;
  f.username.dispatchEvent=()=>{f.form.action='https://other.example/changed';return true;};
  assert.equal(f.bridge.fill(id,'selected','must-not-be-written'),false);
  assert.equal(f.password.value,'synthetic-secret');
}
{
  const f=fixture();const address=f.add('text','street-address');
  const id=f.bridge.discoverAddresses()[0].id;
  address.autocomplete='current-password';
  assert.equal(f.bridge.fillAddress(id,{street:'not-written'}),false);
  assert.equal(address.value,'');
}
console.log('credential form policy tests passed (synthetic DOM; live CEF acceptance separate)');
