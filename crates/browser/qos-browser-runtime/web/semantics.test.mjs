import test from 'node:test';
import assert from 'node:assert/strict';
import {semanticBridge} from './semantics.mjs';
class Element {
  constructor(tag){this.localName=tag;this.dataset={};this.attributes=new Map();this.children=[];this.tabIndex=-1;this.value='';this.selectionStart=0;this.selectionEnd=0;this.scrollTop=0;this.scrollLeft=0;this.listeners=new Map();}
  setAttribute(name,value){this.attributes.set(name,String(value));}
  removeAttribute(name){this.attributes.delete(name);}
  getAttribute(name){return this.attributes.get(name)??null;}
  addEventListener(name,listener){const listeners=this.listeners.get(name)??[];listeners.push(listener);this.listeners.set(name,listeners);}
  dispatchEvent(event){event.target=this;for(const listener of this.listeners.get(event.type)??[])listener(event);return !event.defaultPrevented;}
  setSelectionRange(start,end){this.selectionStart=start;this.selectionEnd=end;}
  append(child){child.remove();this.children.push(child);child.parentNode=this;}
  remove(){if(this.parentNode)this.parentNode.children=this.parentNode.children.filter(child=>child!==this);this.parentNode=null;}
  replaceChildren(){for(const child of this.children)child.parentNode=null;this.children=[];}
}
const event=(type,extra={})=>({type,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;},...extra});
globalThis.document={createElement:tag=>new Element(tag),activeElement:null};
const snapshot=node=>({root:{path:'chat',semantics:{semantic_key:'topic',...node}}});
test('stable semantic node removes cleared topic, role, live and focus state',()=>{
  const root=new Element('div');const bridge=semanticBridge(root,()=>{},()=>{});
  bridge.update(snapshot({role:{Heading:{level:1}},name:'General',description:'Old topic',live:'Polite',state:{focusable:true,selected:true,expanded:true,disabled:true,read_only:true}}));
  const element=root.children[0];
  assert.equal(element.getAttribute('aria-description'),'Old topic');
  assert.equal(element.getAttribute('aria-live'),'polite');assert.equal(element.tabIndex,-1);
  bridge.update(snapshot({role:null,name:'General',state:{focusable:false}}));
  assert.equal(root.children[0],element,'reuse the stable semantic identity');
  for(const attr of ['role','aria-description','aria-live','aria-level','aria-selected','aria-expanded','aria-disabled','aria-readonly'])assert.equal(element.getAttribute(attr),null,attr);
  assert.equal(element.tabIndex,-1);
});
test('removed label does not persist on a stable node',()=>{
  const root=new Element('div');const bridge=semanticBridge(root,()=>{},()=>{});
  bridge.update(snapshot({role:'Group',name:'Old'}));bridge.update(snapshot({role:'Group'}));
  assert.equal(root.children[0].getAttribute('aria-label'),null);
});

test('semantic actions report the runtime acceptance result',()=>{
  const reports=[];const root=new Element('div');
  const bridge=semanticBridge(root,()=>false,report=>reports.push(report));
  bridge.update(snapshot({role:'Button',name:'Send',action_target:'chat.send',state:{focusable:true}}));
  root.children[0].dispatchEvent(event('click'));
  assert.deepEqual(reports.at(-1),{semanticAction:'activate',semanticResult:'rejected'});
});

test('rejected semantic edits roll the DOM mirror back to the canonical value',()=>{
  const reports=[];const root=new Element('div');
  const bridge=semanticBridge(root,()=>false,report=>reports.push(report));
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'saved',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;editor.value='local only';
  editor.dispatchEvent(event('input',{inputType:'insertText',data:'y',isComposing:false}));
  assert.equal(editor.value,'saved');
  assert.deepEqual([editor.selectionStart,editor.selectionEnd],[5,5]);
  assert.deepEqual(reports.at(-1),{semanticAction:'value',semanticResult:'rejected'});
});

test('editable input immediately updates the canonical value and selection in graphemes',()=>{
  const calls=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'',state:{focusable:true,multiline:true},text_selection:{anchor:0,focus:0}}));
  const editor=root.children[0];document.activeElement=editor;
  editor.value='A👨‍👩‍👧‍👦中\nB';
  editor.selectionStart=1;editor.selectionEnd=editor.value.length-1;
  editor.dispatchEvent(event('input',{inputType:'insertText',data:'B',isComposing:false}));
  editor.dispatchEvent(event('select'));
  assert.deepEqual(calls.map(call=>call.slice(2)),[
    ['value','A👨‍👩‍👧‍👦中\nB',0,0],
    ['selection','',1,4],
  ]);
});

test('CJK preedit commits once and ignores the trailing input event',()=>{
  const calls=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;
  editor.dispatchEvent(event('compositionstart'));
  editor.value='draft東京';editor.selectionStart=7;editor.selectionEnd=7;
  editor.dispatchEvent(event('input',{inputType:'insertCompositionText',data:'東京',isComposing:true}));
  editor.dispatchEvent(event('compositionend',{data:'東京'}));
  editor.dispatchEvent(event('input',{inputType:'insertText',data:'東京',isComposing:false}));
  assert.deepEqual(calls.map(call=>call.slice(2)),[
    ['value','draft東京',0,0],
    ['selection','',7,7],
  ]);
});

test('remote canonical edit cancels an unsafe composition without losing committed data',()=>{
  const calls=[];const reports=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},report=>reports.push(report));
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;
  editor.dispatchEvent(event('compositionstart'));
  editor.value='draft東京';
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'remote draft',state:{focusable:true},text_selection:{anchor:12,focus:12}}));
  editor.dispatchEvent(event('compositionend',{data:'東京'}));
  assert.equal(editor.value,'remote draft');
  assert.equal(calls.some(call=>call[2]==='value'),false);
  assert.equal(reports.at(-1).semanticResult,'composition-cancelled-after-canonical-edit');
});

test('canonical multiline updates restore grapheme selection without moving scroll',()=>{
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{});
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'one\ntwo',state:{focusable:true,multiline:true},text_selection:{anchor:0,focus:0}}));
  const editor=root.children[0];document.activeElement=editor;editor.scrollLeft=7;editor.scrollTop=42;
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'A👨‍👩‍👧‍👦中\nB',state:{focusable:true,multiline:true},text_selection:{anchor:1,focus:4}}));
  assert.equal(editor.value,'A👨‍👩‍👧‍👦中\nB');
  assert.deepEqual([editor.selectionStart,editor.selectionEnd],[1,editor.value.length-1]);
  assert.deepEqual([editor.scrollLeft,editor.scrollTop],[7,42]);
});

test('disabled and removed semantic nodes reject stale activation',()=>{
  const calls=[];const reports=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},report=>reports.push(report));
  bridge.update(snapshot({role:'Button',name:'Send',action_target:'chat.send',state:{focusable:true,disabled:true}}));
  const stale=root.children[0];
  stale.dispatchEvent(event('click'));
  bridge.update({root:{path:'chat',children:[]}});
  stale.dispatchEvent(event('click'));
  assert.equal(calls.length,0);
  assert.deepEqual(reports.map(report=>report.semanticResult),['rejected','stale-target']);
});
