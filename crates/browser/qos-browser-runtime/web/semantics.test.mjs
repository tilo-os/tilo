import test from 'node:test';
import assert from 'node:assert/strict';
import {semanticBridge} from './semantics.mjs';
class Element {
  constructor(tag){this.localName=tag;this.dataset={};this.attributes=new Map();this.children=[];this.tabIndex=-1;this.value='';this.selectionStart=0;this.selectionEnd=0;this.scrollTop=0;this.scrollLeft=0;this.listeners=new Map();this.style={cssText:''};}
  setAttribute(name,value){this.attributes.set(name,String(value));}
  removeAttribute(name){this.attributes.delete(name);}
  getAttribute(name){return this.attributes.get(name)??null;}
  addEventListener(name,listener){const listeners=this.listeners.get(name)??[];listeners.push(listener);this.listeners.set(name,listeners);}
  dispatchEvent(event){event.target=this;for(const listener of this.listeners.get(event.type)??[])listener(event);return !event.defaultPrevented;}
  setSelectionRange(start,end){this.selectionStart=start;this.selectionEnd=end;}
  append(...children){for(const child of children){child.remove();this.children.push(child);child.parentNode=this;}}
  contains(node){return node===this||this.children.some(child=>child.contains(node));}
  focus(){document.activeElement=this;this.dispatchEvent(event('focus'));}
  blur(){if(document.activeElement===this)document.activeElement=document.body;this.dispatchEvent(event('blur'));}
  remove(){if(this.parentNode)this.parentNode.children=this.parentNode.children.filter(child=>child!==this);this.parentNode=null;}
  replaceChildren(){for(const child of this.children)child.parentNode=null;this.children=[];}
}
const event=(type,extra={})=>({type,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;},...extra});
globalThis.document={createElement:tag=>new Element(tag),activeElement:null,body:new Element('body')};
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

test('remote canonical edit retains unrebasable composition for explicit recovery',()=>{
  const calls=[];const reports=[];const root=new Element('div');let accept=true;
  const mount=new Element('main');mount.append(root);
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return accept;},report=>reports.push(report));
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;
  editor.dispatchEvent(event('compositionstart'));
  editor.value='draft東京';
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'remote draft',state:{focusable:true},text_selection:{anchor:12,focus:12}}));
  editor.dispatchEvent(event('compositionend',{data:'東京'}));
  assert.equal(editor.value,'remote draft');
  assert.equal(calls.some(call=>call[2]==='value'),false);
  assert.equal(reports.at(-1).semanticResult,'composition-cancelled-after-canonical-edit');
  const recovery=mount.children.find(child=>child.dataset.qosCompositionRecovery!==undefined);
  assert.ok(recovery,'recovery is visible beside the clipped semantic tree');
  assert.equal(recovery.getAttribute('role'),'alert');
  const retained=recovery.children.find(child=>child.localName==='textarea');
  assert.equal(retained.value,'東京','the exact user preedit remains selectable');
  retained.focus();
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'newer remote',state:{focusable:true,focused:true},text_selection:{anchor:12,focus:12}}));
  assert.equal(document.activeElement,retained,'canonical snapshots do not steal focus from recovery');
  assert.equal(editor.value,'newer remote','newer canonical text remains authoritative before recovery');
  const recover=recovery.children.find(child=>child.localName==='button');
  bridge.update({root:{path:'chat',children:[]}});
  assert.equal(recover.disabled,true,'insertion is unavailable while the semantic owner is removed');
  assert.equal(retained.value,'東京','removed owners keep exact text available for copy');
  assert.equal(recovery.children.at(-1).textContent,'Dismiss retained text','the orphaned recovery can be explicitly retired');
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'remounted remote',state:{focusable:true,focused:true},text_selection:{anchor:16,focus:16}}));
  const remounted=root.children[0];
  assert.notEqual(remounted,editor,'the stable target was represented by a new DOM node');
  assert.equal(recover.disabled,false,'same-key remount re-enables explicit insertion');
  accept=false;recover.dispatchEvent(event('click'));
  assert.equal(remounted.value,'remounted remote','rejected recovery restores the remounted canonical value');
  assert.equal(mount.children.includes(recovery),true,'rejected recovery retains the explicit surface');
  accept=true;
  recover.dispatchEvent(event('click'));
  assert.deepEqual(calls.map(call=>call.slice(2)),[
    ['value','remounted remote東京',0,0],
    ['value','remounted remote東京',0,0],
    ['selection','',18,18],
  ]);
  assert.equal(mount.children.includes(recovery),false,'accepted recovery clears the surface');
});

test('canonical focus reconciles one owner without duplicate dispatch or stealing external focus',()=>{
  const calls=[];const mount=new Element('main');const canvas=new Element('canvas');const root=new Element('div');
  mount.append(canvas,root);
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const focused=(order,active)=>({root:{path:'app',children:order.map(name=>({
    path:name,
    semantics:{semantic_key:name,role:'Button',name,action_target:name,state:{focusable:true,focused:name===active}},
  }))}});

  document.activeElement=canvas;
  bridge.update(focused(['one','two'],'one'));
  const one=root.children[0],two=root.children[1];
  assert.equal(document.activeElement,canvas,'canvas keeps the physical keyboard route');
  assert.equal(canvas.getAttribute('aria-activedescendant'),one.id,'canvas exposes the canonical semantic owner');
  assert.equal(canvas.getAttribute('aria-owns'),one.id);
  assert.equal(calls.length,0,'canonical reconciliation does not dispatch a focus action');

  two.focus();
  assert.deepEqual(calls.map(call=>call.slice(1,3)),[['two','focus']]);
  bridge.update(focused(['one','two'],'two'));
  assert.equal(document.activeElement,two);
  assert.equal(calls.length,1,'semantic acknowledgement does not duplicate the action');

  bridge.update(focused(['two','one'],'two'));
  assert.equal(document.activeElement,two,'reorder keeps the stable canonical owner');
  assert.equal(root.children.filter(child=>child.dataset.qosFocused==='true').length,1);

  bridge.update(focused(['three'],'three'));
  const three=root.children[0];
  assert.equal(document.activeElement,three,'remount clears the old owner and focuses the new one');
  assert.equal(calls.length,1);

  const cloudControl=new Element('button');document.body.append(cloudControl);document.activeElement=cloudControl;
  bridge.update(focused(['three'],'three'));
  assert.equal(document.activeElement,cloudControl,'an unrelated Cloud control keeps focus');
  assert.equal(three.dataset.qosFocused,'true','the canonical owner remains represented in the mirror');
  assert.equal(calls.length,1);
});

test('accepted focus survives one acknowledgement snapshot but canonical clear still wins',()=>{
  const calls=[];const mount=new Element('main');const root=new Element('div');mount.append(root);
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const view=focused=>snapshot({role:'TextInput',name:'Message composer',action_target:'chat.composer',value:'',state:{focusable:true,focused},text_selection:{anchor:0,focus:0}});

  document.activeElement=document.body;bridge.update(view(false));
  const editor=root.children[0];editor.focus();
  bridge.update(view(false));
  assert.equal(document.activeElement,editor,'one unacknowledged snapshot cannot cancel accepted focus');
  bridge.update(view(true));
  bridge.update(view(false));
  assert.equal(document.activeElement,document.body,'an acknowledged owner still obeys a genuine canonical clear');

  editor.focus();
  bridge.update(view(false));
  assert.equal(document.activeElement,editor);
  bridge.update(view(false));
  assert.equal(document.activeElement,document.body,'missing acknowledgement cannot retain focus forever');
  assert.deepEqual(calls.map(call=>call.slice(2)),[['focus','',0,0],['focus','',0,0]]);
});

test('separate semantic bridges never treat each other as managed focus',()=>{
  const mountA=new Element('main'),canvasA=new Element('canvas'),rootA=new Element('div');mountA.append(canvasA,rootA);
  const mountB=new Element('main'),canvasB=new Element('canvas'),rootB=new Element('div');mountB.append(canvasB,rootB);
  const callsA=[];const callsB=[];
  const bridgeA=semanticBridge(rootA,(...args)=>{callsA.push(args);return true;},()=>{});
  const bridgeB=semanticBridge(rootB,(...args)=>{callsB.push(args);return true;},()=>{});
  const view=name=>({root:{path:name,semantics:{semantic_key:'focused',role:'Button',name,action_target:name,state:{focusable:true,focused:true}}}});

  document.activeElement=canvasA;bridgeA.update(view('viewer-a'));
  document.activeElement=canvasB;bridgeB.update(view('viewer-b'));
  const focusedB=rootB.children[0];focusedB.focus();
  assert.equal(document.activeElement,focusedB);
  bridgeA.update(view('viewer-a'));
  assert.equal(document.activeElement,focusedB,'viewer A cannot steal viewer B semantic focus');
  assert.equal(callsA.length,0);
  assert.equal(callsB.length,1);
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
