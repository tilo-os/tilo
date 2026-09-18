import test from 'node:test';
import assert from 'node:assert/strict';
import {semanticBridge} from './semantics.mjs';
class Element {
  constructor(tag){this.localName=tag;this.dataset={};this.attributes=new Map();this.children=[];this.tabIndex=-1;}
  setAttribute(name,value){this.attributes.set(name,String(value));}
  removeAttribute(name){this.attributes.delete(name);}
  getAttribute(name){return this.attributes.get(name)??null;}
  addEventListener(){}
  append(child){child.remove();this.children.push(child);child.parentNode=this;}
  remove(){if(this.parentNode)this.parentNode.children=this.parentNode.children.filter(child=>child!==this);this.parentNode=null;}
  replaceChildren(){for(const child of this.children)child.parentNode=null;this.children=[];}
}
globalThis.document={createElement:tag=>new Element(tag),activeElement:null};
const snapshot=node=>({root:{path:'chat',semantics:{semantic_key:'topic',...node}}});
test('stable semantic node removes cleared topic, role, live and focus state',()=>{
  const root=new Element('div');const bridge=semanticBridge(root,()=>{},()=>{});
  bridge.update(snapshot({role:{Heading:{level:1}},name:'General',description:'Old topic',live:'Polite',state:{focusable:true,selected:true,expanded:true,disabled:true,read_only:true}}));
  const element=root.children[0];
  assert.equal(element.getAttribute('aria-description'),'Old topic');
  assert.equal(element.getAttribute('aria-live'),'polite');assert.equal(element.tabIndex,0);
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
