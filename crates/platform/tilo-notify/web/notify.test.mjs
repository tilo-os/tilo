import test from 'node:test';
import assert from 'node:assert/strict';
import {notificationPermission,requestNotification,setActivationListener,showNotification,withdrawNotification} from './notify.mjs';

test('notifications follow the site permission and replace by tag',async()=>{
  const live=[];
  class FakeNotification extends EventTarget{
    static permission='default';
    static async requestPermission(){return FakeNotification.permission=FakeNotification.answer;}
    constructor(title,options){super();Object.assign(this,{title,...options,closed:false});live.push(this);}
    close(){this.closed=true;this.dispatchEvent(new Event('close'));}
  }
  const env={Notification:FakeNotification};
  assert.equal(notificationPermission({Notification:undefined}),'unavailable');
  assert.equal(notificationPermission(env),'default');
  assert.throws(()=>showNotification('t','Title','Body',null,env),/not permitted/);
  FakeNotification.answer='denied';
  await assert.rejects(requestNotification('t','Title','Body',null,env),/denied/);
  FakeNotification.permission='default';FakeNotification.answer='granted';
  await requestNotification('sync','Syncing','1 file',null,env);
  showNotification('sync','Synced','done',null,env);
  assert.deepEqual(live.map(n=>[n.title,n.tag,n.closed]),[['Syncing','sync',true],['Synced','sync',false]]);
  withdrawNotification('sync');
  assert.equal(live[1].closed,true);
});

test('a click brings the page forward and hands over what the notification opens',()=>{
  const live=[];
  class FakeNotification extends EventTarget{
    static permission='granted';
    constructor(title,options){super();Object.assign(this,{title,...options,closed:false});live.push(this);}
    close(){this.closed=true;this.dispatchEvent(new Event('close'));}
  }
  let focused=0;
  const activated=[];
  setActivationListener(uri=>activated.push(uri));
  const env={Notification:FakeNotification,focus:()=>{focused+=1;}};
  showNotification('mention','Ada mentioned you','see you at six','tilo-mention:chat/room',env);
  showNotification('plain','Saved','nothing to open',null,env);
  live[0].dispatchEvent(new Event('click'));
  live[1].dispatchEvent(new Event('click'));
  assert.equal(focused,2);
  assert.deepEqual(activated,['tilo-mention:chat/room']);
  assert.deepEqual(live.map(n=>n.closed),[true,true]);
  setActivationListener(null);
});
