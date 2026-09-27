import test from 'node:test';
import assert from 'node:assert/strict';
import {semanticBridge} from './semantics.mjs';
import {createKeyboardForwarder,modifierSnapshot,semanticKeyboardRoute} from './host-core.mjs';
class Element {
  constructor(tag){this.localName=tag;this.dataset={};this.attributes=new Map();this.children=[];this.tabIndex=-1;this.value='';this.selectionStart=0;this.selectionEnd=0;this.selectionDirection='none';this.scrollTop=0;this.scrollLeft=0;this.listeners=new Map();this.style={cssText:''};}
  setAttribute(name,value){this.attributes.set(name,String(value));}
  removeAttribute(name){this.attributes.delete(name);}
  getAttribute(name){return this.attributes.get(name)??null;}
  getAttributeNames(){return [...this.attributes.keys()];}
  replaceWith(next){const parent=this.parentNode;parent.children[parent.children.indexOf(this)]=next;next.parentNode=parent;this.parentNode=null;if(document.activeElement===this)document.activeElement=document.body;}
  addEventListener(name,listener){const listeners=this.listeners.get(name)??[];listeners.push(listener);this.listeners.set(name,listeners);}
  dispatchEvent(event){event.target??=this;event.currentTarget=this;for(const listener of this.listeners.get(event.type)??[])listener(event);if(event.bubbles&&!event.cancelBubble)this.parentNode?.dispatchEvent(event);return !event.defaultPrevented;}
  setSelectionRange(start,end,direction='none'){this.selectionStart=start;this.selectionEnd=end;this.selectionDirection=direction;}
  append(...children){for(const child of children){child.remove();this.children.push(child);child.parentNode=this;}}
  contains(node){return node===this||this.children.some(child=>child.contains(node));}
  focus(options){this.focusOptions=options;if(document.activeElement!==this)document.activeElement?.blur?.();document.activeElement=this;this.dispatchEvent(event('focus'));}
  blur(){if(document.activeElement===this)document.activeElement=document.body;this.dispatchEvent(event('blur'));}
  remove(){if(this.parentNode)this.parentNode.children=this.parentNode.children.filter(child=>child!==this);this.parentNode=null;}
  replaceChildren(){for(const child of this.children)child.parentNode=null;this.children=[];}
}
const event=(type,extra={})=>({type,bubbles:['click','keydown','keyup','input','select'].includes(type),defaultPrevented:false,cancelBubble:false,preventDefault(){this.defaultPrevented=true;},stopPropagation(){this.cancelBubble=true;},...extra});
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

test('semantic selection preserves native forward, backward, and collapsed directions in canonical graphemes',()=>{
  const calls=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'Ae\u0301👋B',state:{focusable:true},text_selection:{anchor:0,focus:0}}));
  const editor=root.children[0];document.activeElement=editor;
  editor.setSelectionRange(1,5,'forward');editor.dispatchEvent(event('select'));
  editor.setSelectionRange(1,5,'backward');editor.dispatchEvent(event('select'));
  editor.setSelectionRange(5,5,'none');editor.dispatchEvent(event('select'));
  assert.deepEqual(calls.map(call=>call.slice(2)),[
    ['selection','',1,3],
    ['selection','',3,1],
    ['selection','',3,3],
  ]);
});

test('canonical backward UTF-8 selection restores the ordered native range, direction, scroll, and focus',()=>{
  const calls=[];const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'one\ntwo',state:{focusable:true,multiline:true,focused:true},text_selection:{anchor:0,focus:0}}));
  const editor=root.children[0];document.activeElement=editor;editor.scrollLeft=7;editor.scrollTop=42;
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'Ae\u0301👋B',state:{focusable:true,multiline:true,focused:true},text_selection:{anchor:8,focus:1}}));
  assert.deepEqual([editor.selectionStart,editor.selectionEnd,editor.selectionDirection],[1,5,'backward']);
  assert.deepEqual([editor.scrollLeft,editor.scrollTop],[7,42]);
  assert.equal(document.activeElement,editor);
  assert.deepEqual(calls,[]);
});

test('composition cancellation restores canonical backward selection direction',()=>{
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{});
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'Ae\u0301👋B',state:{focusable:true},text_selection:{anchor:8,focus:1}}));
  const editor=root.children[0];editor.dispatchEvent(event('compositionstart'));
  editor.setSelectionRange(0,6,'forward');editor.dispatchEvent(event('compositioncancel'));
  assert.deepEqual([editor.selectionStart,editor.selectionEnd,editor.selectionDirection],[1,5,'backward']);
});

test('semantic editor forwards canonical history keys once and releases modifiers',()=>{
  const dispatched=[];const modifiers=[];let clears=0;
  const shortcuts=createKeyboardForwarder({
    route:semanticKeyboardRoute,
    mac:true,
    dispatch:(type,keyEvent)=>dispatched.push([type,keyEvent.key,keyEvent.shiftKey]),
    publishModifiers:keyEvent=>modifiers.push(modifierSnapshot(keyEvent)),
    clearModifiers:()=>{clears++;},
  });
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},shortcuts);
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'draft',state:{focusable:true,multiline:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;
  const down=event('keydown',{key:'z',code:'KeyZ',metaKey:true,shiftKey:true});
  const up=event('keyup',{key:'z',code:'KeyZ'});
  editor.dispatchEvent(down);editor.dispatchEvent(up);editor.dispatchEvent(up);
  editor.dispatchEvent(event('blur'));
  assert.deepEqual(dispatched,[['keydown','z',true],['keyup','z',undefined]]);
  assert.deepEqual(modifiers,[[false,false,true,true],[false,false,false,false],[false,false,false,false]]);
  assert.equal(clears,1);
  assert.equal(down.defaultPrevented,true);
  assert.equal(up.defaultPrevented,true);
});

test('semantic editor keeps native text, selection, clipboard, browser, and IME keys native',()=>{
  const dispatched=[];
  const shortcuts=createKeyboardForwarder({route:semanticKeyboardRoute,mac:true,dispatch:(...args)=>dispatched.push(args)});
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},shortcuts);
  bridge.update(snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
  const editor=root.children[0];document.activeElement=editor;
  for(const keyEvent of [
    event('keydown',{key:'a',metaKey:true}),
    event('keydown',{key:'c',metaKey:true}),
    event('keydown',{key:'v',metaKey:true}),
    event('keydown',{key:'r',metaKey:true}),
    event('keydown',{key:'Backspace'}),
    event('keydown',{key:'é'}),
  ]){
    editor.dispatchEvent(keyEvent);
    assert.equal(keyEvent.defaultPrevented,false,keyEvent.key);
  }
  editor.dispatchEvent(event('compositionstart'));
  const composingUndo=event('keydown',{key:'z',metaKey:true,isComposing:true});
  editor.dispatchEvent(composingUndo);
  assert.equal(composingUndo.defaultPrevented,false);
  assert.deepEqual(dispatched,[]);
});

test('disabled read-only and stale semantic editors cannot forward history mutations',()=>{
  const dispatched=[];const modifiers=[];let clears=0;
  const shortcuts=createKeyboardForwarder({
    route:semanticKeyboardRoute,
    mac:true,
    dispatch:(type,keyEvent)=>dispatched.push([type,keyEvent.key]),
    publishModifiers:keyEvent=>modifiers.push(modifierSnapshot(keyEvent)),
    clearModifiers:()=>{clears++;},
  });
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},shortcuts);
  const view=state=>snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true,...state},text_selection:{anchor:5,focus:5}});
  bridge.update(view({disabled:true}));
  const disabled=root.children[0];disabled.dispatchEvent(event('keydown',{key:'z',metaKey:true}));
  bridge.update(view({read_only:true}));
  const readOnly=root.children[0];readOnly.dispatchEvent(event('keydown',{key:'z',metaKey:true}));
  bridge.update({root:{path:'chat',children:[]}});
  readOnly.dispatchEvent(event('keydown',{key:'z',metaKey:true}));
  assert.deepEqual(dispatched,[]);
  assert.deepEqual(modifiers,[]);
  assert.equal(clears,0);
});

test('semantic owner releases on read-only transition and stale lifecycle cannot consume a new owner',()=>{
  const dispatched=[];const modifiers=[];let clears=0;
  const shortcuts=createKeyboardForwarder({
    route:semanticKeyboardRoute,
    mac:true,
    dispatch:(type,keyEvent)=>dispatched.push([type,keyEvent.key]),
    publishModifiers:keyEvent=>modifiers.push(modifierSnapshot(keyEvent)),
    clearModifiers:()=>{clears++;},
  });
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},shortcuts);
  const view=(firstState={},secondState={})=>({root:{path:'app',children:[
    {path:'first',semantics:{semantic_key:'first',role:'TextInput',name:'First',action_target:'first',value:'A',state:{focusable:true,...firstState},text_selection:{anchor:1,focus:1}}},
    {path:'second',semantics:{semantic_key:'second',role:'TextInput',name:'Second',action_target:'second',value:'B',state:{focusable:true,...secondState},text_selection:{anchor:1,focus:1}}},
  ]}});
  bridge.update(view());
  const first=root.children[0],second=root.children[1];
  document.activeElement=first;
  first.dispatchEvent(event('keydown',{key:'z',code:'KeyZ',metaKey:true}));
  bridge.update(view({read_only:true}));
  document.activeElement=second;
  second.dispatchEvent(event('keydown',{key:'z',code:'KeyZ',metaKey:true}));
  first.dispatchEvent(event('keyup',{key:'z',code:'KeyZ'}));
  first.dispatchEvent(event('blur'));
  second.dispatchEvent(event('keyup',{key:'z',code:'KeyZ'}));
  assert.deepEqual(dispatched,[['keydown','z'],['keyup','z'],['keydown','z'],['keyup','z']]);
  assert.deepEqual(modifiers,[
    [false,false,false,true],
    [false,false,false,true],
    [false,false,false,false],
  ]);
  assert.equal(clears,1,'only the original owner transition clears before the second owner finishes normally');
});

test('semantic removal and bridge disposal release each outstanding owner once',()=>{
  const dispatched=[];const modifiers=[];let clears=0;
  const shortcuts=createKeyboardForwarder({
    route:semanticKeyboardRoute,
    mac:true,
    dispatch:(type,keyEvent)=>dispatched.push([type,keyEvent.key]),
    publishModifiers:keyEvent=>modifiers.push(modifierSnapshot(keyEvent)),
    clearModifiers:()=>{clears++;},
  });
  const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},shortcuts);
  const view=()=>snapshot({role:'TextInput',name:'Title',action_target:'title',value:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}});
  bridge.update(view());
  const removed=root.children[0];document.activeElement=removed;
  removed.dispatchEvent(event('keydown',{key:'z',code:'KeyZ',metaKey:true}));
  bridge.update({root:{path:'chat',children:[]}});
  bridge.update(view());
  const disposed=root.children[0];document.activeElement=disposed;
  disposed.dispatchEvent(event('keydown',{key:'z',code:'KeyZ',metaKey:true}));
  bridge.close();
  removed.dispatchEvent(event('blur'));
  disposed.dispatchEvent(event('keyup',{key:'z',code:'KeyZ'}));
  assert.deepEqual(dispatched,[['keydown','z'],['keyup','z'],['keydown','z'],['keyup','z']]);
  assert.deepEqual(modifiers,[[false,false,false,true],[false,false,false,true]],'disposed and removed stale events publish no modifiers');
  assert.equal(clears,2);
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
  const recovery=mount.children.find(child=>child.dataset.tiloCompositionRecovery!==undefined);
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
  assert.equal(root.children.filter(child=>child.dataset.tiloFocused==='true').length,1);

  bridge.update(focused(['three'],'three'));
  const three=root.children[0];
  assert.equal(document.activeElement,three,'remount clears the old owner and focuses the new one');
  assert.equal(calls.length,1);

  const cloudControl=new Element('button');document.body.append(cloudControl);document.activeElement=cloudControl;
  bridge.update(focused(['three'],'three'));
  assert.equal(document.activeElement,cloudControl,'an unrelated Cloud control keeps focus');
  assert.equal(three.dataset.tiloFocused,'true','the canonical owner remains represented in the mirror');
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
  bridge.update(snapshot({role:'MultilineTextInput',name:'Message composer',action_target:'chat.composer',value:'A👨‍👩‍👧‍👦中\nB',state:{focusable:true,multiline:true},text_selection:{anchor:1,focus:30}}));
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

test('duplicate tab test IDs route by current path while canonical group identity survives insertion and reorder',()=>{
  const calls=[];const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const group=(id,index)=>({path:`desktop/row/WindowGroup[${index}]/TabBar[0]`,test_id:'tabs',semantics:{role:'TabBar',semantic_key:`tabbar:g${id}`,children:[{role:'Tab',name:`Notes ${id}`,action_target:`tab:${id}`,state:{selected:false}}]}});
  const view=ids=>({root:{path:'desktop',children:ids.map(group)}});
  document.activeElement=document.body;bridge.update(view([10,20]));
  const first=root.children.find(node=>node.children[0]?.getAttribute('aria-label')==='Notes 10');
  const notes=first.children[0];notes.dispatchEvent(event('click'));
  assert.equal(calls.at(-1)[0],'desktop/row/WindowGroup[0]/TabBar[0]','a duplicate test ID cannot choose the other group');
  assert.equal(calls.at(-1)[1],'tab:10');
  bridge.update(view([30,20,10]));
  const retained=root.children.find(node=>node.children[0]?.getAttribute('aria-label')==='Notes 10');
  assert.equal(retained,first,'the existing canonical group retains its DOM owner after insertion/reorder');
  assert.equal(retained.children[0],notes,'the existing tab DOM node survives its changed route path');
  notes.dispatchEvent(event('click'));
  assert.equal(calls.at(-1)[0],'desktop/row/WindowGroup[2]/TabBar[0]');
});


test('caller palette keeps test IDs for identity while actions route by canonical paths',()=>{
  const calls=[];const reports=[];const root=new Element('div');
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},report=>reports.push(report));
  const target='action:exact-certified-host/room/record/revision:insert-table';
  const searchPath='caller-palette/SearchField[0]';
  const field={path:searchPath,semantics:{role:'TextInput',name:'Search',value:'',state:{focusable:true,focused:true}}};
  const results={path:'caller-palette/SearchResultsView[0]',test_id:'action-palette-results',semantics:{role:'List',name:'Search results',children:[{role:'ListItem',name:'Document — Insert table',action_target:target}]}};
  const view=children=>({keyboard_focus_owner:searchPath,root:{path:'caller-palette',test_id:'caller-palette',children}});
  document.activeElement=document.body;bridge.update(view([field,results]));
  assert.equal(document.activeElement,root.children[0],'the private palette publishes its current search-field owner');
  const row=root.children[1].children[0];row.dispatchEvent(event('click'));
  assert.deepEqual(calls.at(-1),['caller-palette/SearchResultsView[0]',target,'activate','',0,0]);
  const editor=root.children[0];document.activeElement=editor;editor.value='Insert table';
  editor.dispatchEvent(event('input',{inputType:'insertText',isComposing:false}));
  assert.deepEqual(calls.find(call=>call[2]==='value'),[searchPath,undefined,'value','Insert table',0,0]);
  bridge.update(view([field,{...results,semantics:{...results.semantics,children:[]}}]));
  const count=calls.length;row.dispatchEvent(event('click'));
  assert.equal(calls.length,count,'revoked row cannot send a stale target into WASM');
  assert.equal(reports.at(-1).semanticResult,'stale-target');
});


test('synthetic leaf focus survives owner acknowledgements and reorder but obeys clear and replacement',()=>{
  const root=new Element('div'),calls=[];document.activeElement=document.body;
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const view=(order,focused,disabled=false)=>({root:{path:'desktop',children:order.map((id,i)=>({
    path:`desktop/group[${i}]/tabs`,test_id:'tabs',semantics:{semantic_key:`tabs:${id}`,role:'TabBar',state:{focused:id===focused,focusable:true},children:[
      {role:'Tab',name:`Notes ${id}`,action_target:`notes:${id}`,state:{focusable:true,disabled}},
    ]},
  }))}});
  bridge.update(view([10,20],undefined));const owner=root.children[0],leaf=owner.children[0];leaf.focus();
  for(let i=0;i<3;i++)bridge.update(view([10,20],10));
  assert.equal(document.activeElement,leaf,'focused owner acknowledges its requested synthetic leaf');
  bridge.update(view([20,10],10));assert.equal(document.activeElement,leaf);
  leaf.dispatchEvent(event('click'));assert.equal(calls.at(-1)[0],'desktop/group[1]/tabs');
  bridge.update(view([20,10],20));assert.equal(document.activeElement,root.children.find(e=>e!==owner));
  leaf.focus();bridge.update(view([20,10],10));bridge.update(view([20,10],undefined));
  assert.equal(document.activeElement,document.body,'canonical clear retires acknowledged leaf');
  leaf.focus();bridge.update(view([20,10],10,true));assert.notEqual(document.activeElement,leaf);
  bridge.close();
});

test('focused custom app owner preserves accepted expression and button focus across snapshots',()=>{
  const root=new Element('div'),calls=[];document.activeElement=document.body;
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const owner='desktop/calculator';
  const view=(value,focused=true)=>({keyboard_focus_owner:focused?owner:null,root:{
    path:owner,semantics:{role:'Group',name:'Calculator',state:{focusable:true,focused},children:[
      {role:'TextInput',name:'Expression',value,action_target:'calculator.expression',state:{focusable:true,focused:false}},
      {role:'Button',name:'Evaluate expression',action_target:'calculator.evaluate',state:{focusable:true,focused:false}},
    ]},
  }});
  bridge.update(view('2'));
  const group=root.children[0],expression=group.children[0],evaluate=group.children[1];
  expression.focus();
  assert.deepEqual(calls.at(-1),[owner,'calculator.expression','focus','',0,0]);
  bridge.update(view(''));
  assert.equal(expression.value,'','the editable canonical value stays empty after clear');
  bridge.update(view('2^2'));
  assert.equal(document.activeElement,expression);
  assert.equal(expression.value,'2^2');
  evaluate.focus();
  assert.deepEqual(calls.at(-1),[owner,'calculator.evaluate','focus','',0,0]);
  bridge.update(view('2^2'));
  assert.equal(document.activeElement,evaluate);
  bridge.update(view('2^2',false));
  assert.equal(document.activeElement,document.body);
  bridge.close();
});

test('focused retained child beats focused aggregate and reconciliation prevents scrolling',()=>{
  const root=new Element('div');document.activeElement=document.body;
  const bridge=semanticBridge(root,()=>true,()=>{});
  const view={root:{path:'app',semantics:{role:'Group',state:{focused:true}},children:[{path:'app/editor',semantics:{role:'TextInput',name:'Title',state:{focused:true,focusable:true},value:'draft'}}]}};
  bridge.update(view);const editor=root.children[1];
  assert.equal(document.activeElement,editor);assert.deepEqual(editor.focusOptions,{preventScroll:true});
  const outside=new Element('button');outside.focus();bridge.update(view);assert.equal(document.activeElement,outside);
  bridge.close();
});

test('bubbling activation invokes only the target and div roles have guarded keyboard activation',()=>{
  const root=new Element('div'),calls=[];document.activeElement=document.body;
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return args[1]!=='close';},()=>{});
  bridge.update(snapshot({role:'TabBar',state:{focusable:true},children:[{role:'Tab',action_target:'notes',state:{focusable:true},children:[{role:'Button',action_target:'close',state:{focusable:true}}]}]}));
  const tab=root.children[0].children[0],close=tab.children[0];
  close.dispatchEvent(event('click'));assert.deepEqual(calls.map(c=>c[1]),['close'],'rejected child must not activate ancestor');
  tab.focus();calls.length=0;
  for(const key of ['Enter',' ']){const down=event('keydown',{key});tab.dispatchEvent(down);assert.equal(down.defaultPrevented,true);}
  assert.deepEqual(calls.map(c=>c.slice(1,3)),[['notes','activate'],['notes','activate']]);
  for(const extra of [{isComposing:true},{ctrlKey:true},{altKey:true},{metaKey:true},{shiftKey:true},{repeat:true}])tab.dispatchEvent(event('keydown',{key:'Enter',...extra}));
  assert.equal(calls.length,2);
  close.focus();calls.length=0;
  const nativeEnter=event('keydown',{key:'Enter'});close.dispatchEvent(nativeEnter);
  assert.equal(nativeEnter.defaultPrevented,false,'native button retains native default click');
  close.dispatchEvent(event('click'));assert.deepEqual(calls.map(c=>c[1]),['close']);
  bridge.close();
});

test('semantic Enter forwards one shared key pair; Shift newline and IME stay single-owner',()=>{
  const dispatched=[],calls=[];
  const shortcuts=createKeyboardForwarder({route:semanticKeyboardRoute,dispatch:(type,e)=>dispatched.push([type,e.key,Boolean(e.shiftKey)])});
  const root=new Element('div');document.activeElement=document.body;
  const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},shortcuts);
  bridge.update(snapshot({role:'MultilineTextInput',action_target:'composer',state:{focusable:true},value:'draft'}));
  const editor=root.children[0];editor.focus();calls.length=0;
  for(const shiftKey of [false,true]){
    const down=event('keydown',{key:'Enter',code:'Enter',shiftKey});editor.dispatchEvent(down);assert.equal(down.defaultPrevented,true);
    editor.dispatchEvent(event('keyup',{key:'Enter',code:'Enter',shiftKey}));
  }
  editor.dispatchEvent(event('compositionstart'));
  const composing=event('keydown',{key:'Enter',isComposing:true});editor.dispatchEvent(composing);assert.equal(composing.defaultPrevented,false);
  assert.deepEqual(dispatched,[['keydown','Enter',false],['keyup','Enter',false],['keydown','Enter',true],['keyup','Enter',true]]);
  assert.equal(calls.filter(c=>c[2]==='value').length,0,'no duplicate DOM insertion');
  bridge.close();
});

test('semantic keyboard action revalidates exact focus before forwarding to a mounted app',()=>{
  const dispatched=[];let accepted=true;
  const shortcuts=createKeyboardForwarder({route:semanticKeyboardRoute,dispatch:(type,event)=>dispatched.push([type,event.key])});
  const root=new Element('div');document.activeElement=document.body;
  const bridge=semanticBridge(root,()=>accepted,()=>{},shortcuts);
  bridge.update(snapshot({role:'MultilineTextInput',action_target:'composer',state:{focusable:true},value:'draft'}));
  const editor=root.children[0];editor.focus();accepted=false;
  const enter=event('keydown',{key:'Enter'});editor.dispatchEvent(enter);
  assert.equal(enter.defaultPrevented,true,'a rejected application key cannot insert local-only text');
  assert.deepEqual(dispatched,[],'a revoked focus route cannot send a key to a neighboring application');
  bridge.close();
});

test('authoritative retained focus separates independent apps with locally focused editors',()=>{
  const root=new Element('div');document.activeElement=document.body;
  const bridge=semanticBridge(root,()=>true,()=>{});
  const view=owner=>({keyboard_focus_owner:owner,root:{path:'desktop',children:[
    {path:'desktop/first',semantics:{role:'MultilineTextInput',semantic_key:'first',state:{focusable:true,focused:true},value:'first'}},
    {path:'desktop/second',semantics:{role:'TextInput',semantic_key:'second',state:{focusable:true,focused:true},value:'second'}},
  ]}});
  bridge.update(view('desktop/first'));
  const [first,second]=root.children;second.focus();
  for(let i=0;i<3;i++)bridge.update(view('desktop/second'));
  assert.equal(document.activeElement,second,'other app local focus cannot replace acknowledged native owner');
  bridge.update(view('desktop/first'));assert.equal(document.activeElement,first,'actual canonical owner change wins');
  const cleared=view('desktop/first');cleared.root.children[0].semantics.state.focused=false;bridge.update(cleared);
  assert.equal(document.activeElement,document.body,'an owner with cleared focus must not fall through to another app');
  bridge.close();
});

test('authoritative no-owner clears managed focus without choosing another app or stealing external focus',()=>{
  const root=new Element('div');document.activeElement=document.body;
  const bridge=semanticBridge(root,()=>true,()=>{});
  const view=owner=>({keyboard_focus_owner:owner,root:{path:'desktop',children:[
    {path:'desktop/chat',semantics:{role:'MultilineTextInput',state:{focusable:true,focused:true},value:'draft'}},
    {path:'desktop/notes',semantics:{role:'TextInput',state:{focusable:true,focused:owner==='desktop/notes'},value:'Title'}},
  ]}});
  bridge.update(view('desktop/notes'));
  assert.equal(document.activeElement,root.children[1]);
  for(let i=0;i<3;i++)bridge.update(view(null));
  assert.equal(document.activeElement,document.body,'explicit no-owner must not adopt Chat local focus');
  const external=new Element('input');external.focus();bridge.update(view(null));
  assert.equal(document.activeElement,external,'canonical clear preserves external focus');
  document.activeElement=document.body;
  const legacy=view(null);delete legacy.keyboard_focus_owner;
  bridge.update(legacy);
  assert.equal(document.activeElement,root.children[0],'legacy snapshots still use their published focused state');
  bridge.close();
});

test('a throttled status sequence leaves the accessibility DOM untouched until the final tree arrives intact',()=>{
  const root=new Element('div');document.activeElement=document.body;
  const calls=[];const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{});
  const view=(value,focus)=>({keyboard_focus_owner:'desktop/writer',root:{path:'desktop',children:[
    {path:'desktop/writer',semantics:{role:'MultilineTextInput',state:{focusable:true,focused:true,multiline:true},value,text_selection:{anchor:focus,focus}}},
    {path:'desktop/title',semantics:{role:'TextInput',state:{focusable:true},value:'Title'}},
  ]}});
  // The host applies a status's tree only when the status carries one.
  const apply=status=>{if(status.semantics)bridge.update(status.semantics);};
  apply({phase:'presented',semantics:view('a',1)});
  const [editor,title]=root.children;
  assert.equal(document.activeElement,editor);
  assert.equal(editor.value,'a');
  // Two throttled statuses: the tree changed on the runtime side but was not sent.
  apply({phase:'presented',frames:2});apply({phase:'presented',frames:3});
  assert.deepEqual(root.children,[editor,title],'no DOM churn for statuses without a tree');
  assert.equal(editor.value,'a');assert.deepEqual([editor.selectionStart,editor.selectionEnd],[1,1]);
  assert.equal(document.activeElement,editor);
  // The deadline delivers the final tree: value and caret catch up in one step.
  apply({phase:'presented',frames:4,semantics:view('abcd',4)});
  assert.deepEqual(root.children,[editor,title],'the same elements are reused');
  assert.equal(editor.value,'abcd');assert.deepEqual([editor.selectionStart,editor.selectionEnd],[4,4]);
  assert.equal(editor.tiloState.canonicalValue,'abcd');
  assert.equal(document.activeElement,editor);
  assert.deepEqual(calls,[],'catching up never dispatches actions back to the runtime');
  bridge.close();
});

test('retained outgoing semantic editor cannot refocus or edit after ownership changes before a snapshot',()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const calls=[],keys=[];
 const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{}, {keydown:(_el,e,_c,_m,authorize)=>{if(authorize())keys.push(e.key);}},scope);
 bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));
 const editor=root.children[0];document.activeElement=editor;
 current={sequence:'2',activation:'2',owner:'calculator'};
 editor.dispatchEvent(event('keydown',{key:'Enter'}));editor.value='leaked';editor.dispatchEvent(event('input'));editor.setSelectionRange(0,0);editor.dispatchEvent(event('select'));
 assert.deepEqual(calls,[]);assert.deepEqual(keys,[]);assert.equal(editor.value,'saved');
});
test('semantic composition revoked by activation drains silently and a new owner session works',()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const calls=[];
 const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},undefined,scope);
 bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true}}));const editor=root.children[0];document.activeElement=editor;
 editor.dispatchEvent(event('compositionstart'));editor.value='saved old';current={sequence:'2',activation:'2',owner:'other'};
 editor.dispatchEvent(event('compositionend',{data:'old'}));editor.dispatchEvent(event('input'));assert.deepEqual(calls,[]);assert.equal(editor.value,'saved');assert.equal(root.children.length,1);
 current={sequence:'3',activation:'3',owner:'chat'};const fresh=root.children[0];fresh.dispatchEvent(event('compositionstart'));fresh.value='saved new';fresh.dispatchEvent(event('compositionend',{data:'new'}));assert.equal(calls[0][2],'value');assert.equal(calls[0][3],'saved new');
});

test('a fresh key gesture resumes semantic editing after same-owner activation cancels composition',()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const keys=[];
 const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},{keydown:(_el,e,_c,_m,authorize)=>{if(authorize())keys.push(e.key);}},scope);
 bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true}}));const editor=root.children[0];document.activeElement=editor;
 editor.dispatchEvent(event('compositionstart'));current={sequence:'2',activation:'2',owner:'chat'};editor.dispatchEvent(event('compositionend',{data:'old'}));
 root.children[0].dispatchEvent(event('keydown',{key:'Enter'}));assert.deepEqual(keys,['Enter']);
});
test('fresh ownership prevents a stale semantic snapshot from restoring outgoing DOM focus',()=>{
 let current={sequence:'2',activation:'2',owner:null};const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},undefined,scope);document.activeElement=document.body;
 bridge.update({...snapshot({role:'TextInput',value:'old',state:{focusable:true,focused:true}}),keyboard_focus_owner:'chat'});
 assert.equal(document.activeElement,document.body);
});
test('drained semantic composition cancellation permits fresh soft-keyboard input',async()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const calls=[];const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},undefined,scope);
 bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true}}));const editor=root.children[0];document.activeElement=editor;
 editor.dispatchEvent(event('compositionstart'));current={sequence:'2',activation:'2',owner:'chat'};editor.dispatchEvent(event('compositionend',{data:'old'}));editor.dispatchEvent(event('input'));assert.deepEqual(calls,[]);
 await new Promise(resolve=>setTimeout(resolve,5));const fresh=root.children[0];fresh.value='saved new';fresh.dispatchEvent(event('input',{inputType:'insertText',data:'new'}));assert.equal(calls[0]?.[3],'saved new');
});
test('revoked semantic DOM lifetime rejects delayed tails after same-owner reselection and away/back',async()=>{
 for(const away of [false,true]){
  let current={sequence:'1',activation:'1',owner:'chat'};const calls=[];const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
  const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},undefined,scope);
  bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true}}));const old=root.children[0];document.activeElement=old;old.dispatchEvent(event('compositionstart'));
  if(away)current={sequence:'2',activation:'2',owner:'other'};
  current={sequence:away?'3':'2',activation:away?'3':'2',owner:'chat'};
  old.value='saved same';old.dispatchEvent(event('compositionend',{data:'same'}));await new Promise(resolve=>setTimeout(resolve,5));old.value='saved same';old.dispatchEvent(event('input',{data:'same'}));assert.deepEqual(calls,[]);
  const fresh=root.children[0];assert.notEqual(fresh,old);fresh.value='saved same';fresh.dispatchEvent(event('input',{inputType:'insertText',data:'same'}));assert.equal(calls[0]?.[3],'saved same');
 }
});
test('snapshot cancellation reconciles focus onto the replacement semantic editor',()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,()=>true,()=>{},undefined,scope);
 const view={...snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true,focused:true}}),keyboard_focus_owner:'chat'};
 bridge.update(view);const old=root.children[0];document.activeElement=old;old.dispatchEvent(event('compositionstart'));current={sequence:'2',activation:'2',owner:'chat'};bridge.update(view);
 assert.notEqual(root.children[0],old);assert.equal(document.activeElement,root.children[0]);assert.equal(root.children[0].dataset.tiloFocused,'true');
});
test('retired semantic editors reject focus, selection and keyboard release callbacks',()=>{
 let current={sequence:'1',activation:'1',owner:'chat'};const calls=[],keys=[];const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence,owns:path=>current.owner===path};
 const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},{keyup:()=>keys.push('up'),blur:()=>keys.push('blur')},scope);
 bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true}}));const old=root.children[0];old.dispatchEvent(event('compositionstart'));current={sequence:'2',activation:'2',owner:'chat'};old.dispatchEvent(event('compositionend',{data:'old'}));
 for(const type of ['focus','select','keyup','blur'])old.dispatchEvent(event(type,{key:'Enter'}));assert.deepEqual(calls,[]);assert.deepEqual(keys,[]);
});
test('plain semantic editor lifetime is revoked on same-owner reselection and away/back',()=>{
 for(const away of [false,true]){
  let current={sequence:'1',activation:'1',owner:'chat'};const calls=[];const scope={capture:()=>({...current}),valid:s=>s.sequence===current.sequence&&s.activation===current.activation&&s.owner===current.owner,owns:path=>current.owner===path};
  const root=new Element('div');const bridge=semanticBridge(root,(...args)=>{calls.push(args);return true;},()=>{},undefined,scope);
  bridge.update(snapshot({role:'TextInput',value:'saved',action_target:'draft',state:{focusable:true},text_selection:{anchor:5,focus:5}}));const old=root.children[0];document.activeElement=old;
  if(away){current={sequence:'2',activation:'2',owner:'other'};bridge.revokeInput();}
  current={sequence:away?'3':'2',activation:away?'3':'2',owner:'chat'};bridge.revokeInput();
  old.value='stale';old.dispatchEvent(event('input',{inputType:'insertText',data:'stale'}));old.setSelectionRange(0,0);old.dispatchEvent(event('select'));
  assert.deepEqual(calls,[]);assert.notEqual(root.children[0],old);
  const fresh=root.children[0];fresh.value='fresh';fresh.dispatchEvent(event('input',{inputType:'insertText',data:'fresh'}));assert.equal(calls[0]?.[3],'fresh');
 }
});

test('unique test ids do not replace the canonical keyboard owner for text actions',()=>{
 const root=new Element('div'),calls=[];const owner='CertifiedDesktop[0]/DesktopSurface[0]/CalculatorApp[0]';
 const bridge=semanticBridge(root,(...args)=>{calls.push(args);return args[0]===owner;},()=>{});
 bridge.update({keyboard_focus_owner:owner,root:{path:owner,test_id:'calculator-app',semantics:{role:'TextInput',name:'Expression',action_target:'calculator.expression',value:'2',state:{focusable:true,focused:true}}}});
 const editor=root.children[0];editor.focus();editor.value='2^2';editor.dispatchEvent(event('input',{inputType:'insertText',data:'2'}));editor.setSelectionRange(1,2);editor.dispatchEvent(event('select'));
 assert(calls.length>=3);assert(calls.every(x=>x[0]===owner));assert(calls.some(x=>x[2]==='focus')&&calls.some(x=>x[2]==='value')&&calls.some(x=>x[2]==='selection'));bridge.close();
});
