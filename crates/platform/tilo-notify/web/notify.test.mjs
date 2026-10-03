import test from 'node:test';
import assert from 'node:assert/strict';
import {notificationPermission,requestNotification,showNotification,withdrawNotification} from './notify.mjs';

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
  assert.throws(()=>showNotification('t','Title','Body',env),/not permitted/);
  FakeNotification.answer='denied';
  await assert.rejects(requestNotification('t','Title','Body',env),/denied/);
  FakeNotification.permission='default';FakeNotification.answer='granted';
  await requestNotification('sync','Syncing','1 file',env);
  showNotification('sync','Synced','done',env);
  assert.deepEqual(live.map(n=>[n.title,n.tag,n.closed]),[['Syncing','sync',true],['Synced','sync',false]]);
  withdrawNotification('sync');
  assert.equal(live[1].closed,true);
});
