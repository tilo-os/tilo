import init,* as runtime from './pkg/qos_browser_runtime.js';
import {MountRegistry,wheelPixels} from './host-core.mjs';
import {acquireStorage} from './storage.mjs';
import {semanticBridge} from './semantics.mjs';
import {settleComponentClose} from './close-recovery.mjs';
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
let initialized,fontsPromise;
const fonts=()=>fontsPromise??=fetch(new URL('./fonts.json',import.meta.url)).then(r=>r.json()).then(async names=>Object.fromEntries(await Promise.all(names.map(async name=>[name,new Uint8Array(await (await fetch(new URL(`./fonts/${name}`,import.meta.url))).arrayBuffer())]))));
const ready=()=>initialized??=init();
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='qos-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;';
  const surface=document.createElement('div');surface.className='qos-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:hidden';
  const editor=document.createElement('textarea');editor.setAttribute('aria-label','qOS focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none';
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','qOS application');accessibility.style.cssText='position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);';
  surface.append(canvas,editor,accessibility);target.append(surface);
  let id,storage,closed=false,composing=false,pasteShift=false,flush=Promise.resolve(),lastState,readyResolve,readyReject,persistenceFailed=false;
  const presented=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  void presented.catch(()=>{});
  const report=data=>options.onStatus?.(data);
  const config={app:'writer',profile:'renderer-proof',device:'browser-proof-device',room:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',author:'Browser proof user',instance:1,mac:/Mac/.test(navigator.platform),...options};
  delete config.onStatus;delete config.signal;delete config.loadAsset;
  const semantic=semanticBridge(accessibility,(...args)=>runtime.semantic(id,...args),report);
  const listeners=new AbortController();const on=(node,type,fn,opts={})=>node.addEventListener(type,fn,{...opts,signal:listeners.signal});
  function active(){return !closed&&registry.current(token)&&id!==undefined;}
  function update(raw){
    if(closed||!registry.current(token))return;
    const state=JSON.parse(raw);lastState={...lastState,...state};report(state);
    surface.dataset.phase=state.phase;surface.dataset.frames=String(state.frames??0);
    if(state.phase==='failed')readyReject(Error(state.error));
    if(state.semantics)semantic.update(state.semantics);
    if(state.ime){
      editor.style.left=`${state.ime.x}px`;editor.style.top=`${state.ime.y}px`;editor.style.height=`${Math.max(1,state.ime.height)}px`;
      if(document.activeElement===canvas)editor.focus({preventScroll:true});
    }else if(document.activeElement===editor){canvas.focus({preventScroll:true});}
    if(state.phase==='presented'){readyResolve();void persist().catch(()=>{});void loadAssets();}
  }
  async function loadAssets(){
    if(!active())return;
    for(const request of JSON.parse(runtime.asset_requests(id))){
      try{
        let bytes;
        if(options.loadAsset)bytes=await options.loadAsset(request.url,listeners.signal);
        else {
          const url=new URL(request.url,location.href);
          if(url.origin!==location.origin||!['http:','https:'].includes(url.protocol))throw Error('asset unavailable: no admitted content transport');
          const response=await fetch(url,{signal:listeners.signal,credentials:'same-origin'});
          if(!response.ok)throw Error(`asset HTTP ${response.status}`);
          bytes=new Uint8Array(await response.arrayBuffer());
        }
        if(active())runtime.complete_asset(id,request.loader,request.url,bytes);
      }catch(error){if(active()){runtime.complete_asset(id,request.loader,request.url,undefined);report({asset:'unavailable',assetUrl:request.url,error:String(error)});}}
    }
  }
  async function persist(){
    if(!active()||!storage||persistenceFailed)return;
    const current=id;
    flush=flush.catch(()=>{}).then(async()=>{
      if(!active()||persistenceFailed)return;
      const writes=JSON.parse(runtime.pending_writes(current));
      if(!writes.length)return;
      try{
        await storage.commit(writes);
        for(const w of writes)runtime.acknowledge_storage(config.profile,config.device,BigInt(w.revision),undefined);
        report({persistence:'committed',pendingWrites:0,error:null});
      }catch(error){
        for(const w of writes)runtime.acknowledge_storage(config.profile,config.device,BigInt(w.revision),String(error));
        persistenceFailed=true;report({persistence:'failed',pendingWrites:JSON.parse(runtime.pending_writes(current)).length,error:String(error)});throw error;
      }
    });
    return flush;
  }
  let cleanupPromise;
  function cleanup(){
    return cleanupPromise??=(async()=>{
    closed=true;listeners.abort();readyReject(Error('mount disposed'));
    let writes=[];
    if(id!==undefined)writes=JSON.parse(runtime.close(id));
    semantic.close();surface.remove();
    try{
      await flush.catch(()=>{});
      if(storage&&id!==undefined)await settleComponentClose(runtime,id,storage,config.profile,config.device);
      report({phase:'closed',persistence:'committed'});
    }catch(error){
      // Keep actor custody and the immutable pending bytes until durable retry.
      const retained=storage;storage=undefined;
      const retry=async()=>{
        await settleComponentClose(runtime,id,retained,config.profile,config.device);
        await retained.release();recoveryJobs.delete(retry);
        report({phase:'closed',persistence:'committed',pendingWrites:0,error:null});
      };
      if(retained)recoveryJobs.add(retry);
      report({phase:'closed',persistence:'failed',pendingWrites:writes.length,recoveryAvailable:!!retained,error:String(error)});throw error;
    }
    finally{if(storage)await storage.release();closures.delete(cleanup);}
    })();
  }
  closures.add(cleanup);
  const host={close:()=>{void cleanup().catch(error=>report({error:String(error)}));}};
  registry.finish(token,host);
  if(options.signal){
    if(options.signal.aborted)registry.remove(token);
    else on(options.signal,'abort',()=>registry.remove(token),{once:true});
  }
  try{
    if(!navigator.gpu)throw Error('WebGPU is required for the qOS Vello compute renderer');
    await ready();if(!registry.current(token))throw Error('mount disposed');
    const acquired=await acquireStorage(config.profile,config.device);
    if(closed||!registry.current(token)){await acquired.release();throw Error("mount disposed");}
    storage=acquired;
    if(storage.fresh){runtime.refresh_storage_lease(config.profile,config.device);storage.fresh=false;}
    const [assets,entries]=await Promise.all([fonts(),storage.hydrate()]);
    if(!registry.current(token))throw Error('mount disposed');
    if(config.app==='writer'&&!config.record&&!config.newDocument){
      const found=entries.map(e=>JSON.parse(e.key)).find(k=>k.type==='record'&&k.package==='dev.qos.word-processor'&&k.workspace===config.room);
      config.record=found?.record;
    }
    id=runtime.mount(canvas,JSON.stringify(config),assets,entries,raw=>queueMicrotask(()=>update(raw)));
    for(const node of [canvas,editor]){
      on(node,'keydown',event=>{
        if(active())runtime.modifiers(id,event.ctrlKey,event.altKey,event.shiftKey,event.metaKey);
        if(!composing&&(event.ctrlKey||event.metaKey)&&['v','c','x'].includes(event.key.toLowerCase())){pasteShift=event.shiftKey;event.stopImmediatePropagation();}
      },{capture:true});
      for(const kind of ['paste','copy','cut'])on(node,kind,event=>{
        if(!active()||!event.clipboardData||composing)return;
        try{if(kind==='paste'&&!event.clipboardData.getData('text/plain')){event.preventDefault();report({clipboard:'empty or unsupported clipboard payload'});return;}const completed=runtime.clipboard_event(id,kind,event.clipboardData,pasteShift);event.preventDefault();report({lastInput:kind,clipboard:completed?'completed in trusted DOM event':'unavailable or no selection'});}
        catch(error){report({clipboard:'failed',error:String(error)});}
      });
    }
    on(editor,'keydown',event=>{
      if(composing||event.isComposing)return;
      if((event.ctrlKey||event.metaKey)&&['v','c','x'].includes(event.key.toLowerCase()))return;
      event.preventDefault();canvas.dispatchEvent(new KeyboardEvent('keydown',event));report({lastInput:'keyboard'});
    });
    on(editor,'keyup',event=>{if(!composing){event.preventDefault();canvas.dispatchEvent(new KeyboardEvent('keyup',event));}});
    on(editor,'compositionstart',()=>{composing=true;runtime.ime(id,'start','');report({lastInput:'compositionstart'});});
    on(editor,'compositionupdate',event=>{runtime.ime(id,'preedit',event.data);});
    on(editor,'compositionend',event=>{runtime.ime(id,'preedit','');runtime.ime(id,'commit',event.data);composing=false;editor.value='';report({lastInput:'compositioncommit'});});
    on(editor,'beforeinput',event=>{if(composing||event.isComposing)return;if(event.inputType==='insertFromPaste'){event.preventDefault();return;}if(event.inputType==='insertText'&&event.data){event.preventDefault();runtime.ime(id,'commit',event.data);editor.value='';report({lastInput:'beforeinput'});}});
    on(editor,'focus',()=>{canvas.dispatchEvent(new FocusEvent('focus'));if(active())runtime.ime(id,'start','');});
    on(editor,'blur',()=>{if(active()){runtime.ime(id,'end','');canvas.dispatchEvent(new FocusEvent('blur'));}});
    on(canvas,'pointerdown',()=>report({lastInput:'pointer'}));
    on(canvas,'wheel',event=>{
      if(event.deltaMode===2){event.preventDefault();const [x,y]=wheelPixels(event.deltaX,event.deltaY,2,canvas.clientWidth,canvas.clientHeight);canvas.dispatchEvent(new WheelEvent('wheel',{deltaX:-x,deltaY:-y,deltaMode:0,ctrlKey:event.ctrlKey,shiftKey:event.shiftKey}));}
      report({lastInput:'wheel'});
    },{passive:false});
    on(document,'visibilitychange',()=>{if(active())runtime.visible(id,document.visibilityState==='visible');});
    await presented;
    return {cleanup:async()=>{registry.remove(token);await cleanup();},flush:()=>{persistenceFailed=false;return persist();},
      state:()=>lastState,canvas,editor,semantics:accessibility,
      durableState:()=>acquired.hydrate(),probePixels:async()=>JSON.parse(await runtime.probe_pixels(id)),failNextWrite:()=>{storage.abortNext=true;},destroyDevice:()=>runtime.destroy_device(id),recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args)};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
export function pendingComponentCloseCount(){return runtime.component_close_count();}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
