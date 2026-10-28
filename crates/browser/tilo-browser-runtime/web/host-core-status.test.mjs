import test from 'node:test';
import assert from 'node:assert/strict';
import {createHostStatus,HOST_STATUS_LIFETIME_MS} from './host-core.mjs';

function clock(){
  const timers=new Map();let next=1;
  return {
    setTimer(run,ms){const id=next++;timers.set(id,{run,ms});return id;},
    clearTimer(id){timers.delete(id);},
    elapse(){const due=[...timers.values()];timers.clear();for(const t of due)t.run();return due.map(t=>t.ms);},
    get pending(){return timers.size;},
  };
}
const shown=element=>element.style.cssText.startsWith('display:block')&&!element.style.cssText.includes('clip-path');

test('a page status hides on its own after the notice lifetime',()=>{
  const element={style:{},textContent:''},time=clock();
  const status=createHostStatus(element,time);
  assert.equal(shown(element),false);
  status.show('Desktop request unavailable: offline');
  assert.equal(shown(element),true);assert.equal(element.textContent,'Desktop request unavailable: offline');
  assert.deepEqual(time.elapse(),[HOST_STATUS_LIFETIME_MS]);
  assert.equal(shown(element),false,'nothing stays over the desktop once its lifetime is over');
  assert.equal(element.textContent,'');
});

test('a status repeated with every frame is not shown again, a new event is',()=>{
  const element={style:{},textContent:''},time=clock();
  const status=createHostStatus(element,time);
  status.show('Desktop change saved');time.elapse();
  status.show('Desktop change saved');
  assert.equal(shown(element),false,'the same retained status on the next frame does not come back');
  assert.equal(time.pending,0);
  status.show('Could not copy to system clipboard: denied',{again:true});
  status.show('Could not copy to system clipboard: denied',{again:true});
  assert.equal(shown(element),true);assert.equal(time.pending,1,'a second event restarts the one timer');
});

test('a finished copy is spoken, not painted over the window under the corner',()=>{
  const element={style:{},textContent:''},time=clock();
  const status=createHostStatus(element,time);
  status.show('Copied to system clipboard',{spoken:true,again:true});
  assert.equal(element.textContent,'Copied to system clipboard','screen readers still hear it');
  assert.equal(shown(element),false);
  assert.match(element.style.cssText,/clip-path:inset\(50%\)/);
  time.elapse();assert.equal(element.textContent,'');
});
