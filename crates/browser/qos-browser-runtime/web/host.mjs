import {createDesktopNavigation} from './desktop-navigation.mjs';
import {bindActionPalette} from './action-palette-bridge.mjs';
import {bindDesktopCommands} from './desktop-command-bridge.mjs';
import {commitPeerBatches,readPeerBatch} from './peer-persistence.mjs';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
import init,* as runtime from './pkg/qos_browser_runtime.js';
import {MountRegistry,beforeInputRoute,createKeyboardForwarder,keyboardRoute,modifierSnapshot,overlayKeyboardRoute,routeGlobalSearchShortcut,wheelPixels} from './host-core.mjs';
import {acquireStorage} from './storage.mjs';
import {semanticBridge} from './semantics.mjs';
import {settleComponentClose,recoverableClose} from './close-recovery.mjs';
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
const fetchRequired=async url=>{const response=await fetch(url);if(!response.ok)throw Error(`asset HTTP ${response.status}`);return response;};
const fonts=retryableMemo(async()=>{
 const names=await (await fetchRequired(new URL('./fonts.json',import.meta.url))).json();
 return Object.fromEntries(await Promise.all(names.map(async name=>[name,new Uint8Array(await (await fetchRequired(new URL(`./fonts/${name}`,import.meta.url))).arrayBuffer())])));
});
const ready=retryableMemo(()=>init());
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='qos-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;';
  // Native focus may scroll hidden semantic controls; it must not move the canvas viewport.
  const surface=document.createElement('div');surface.className='qos-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:clip';
  const editor=document.createElement('textarea');editor.setAttribute('aria-label','qOS focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none';
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','qOS application');accessibility.style.cssText='position:absolute;top:0;left:0;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);';
  const commandStatus=document.createElement('div');commandStatus.setAttribute('role','status');commandStatus.setAttribute('aria-live','polite');
  commandStatus.style.cssText='display:none;position:absolute;bottom:12px;left:12px;max-width:80%;padding:8px 12px;background:#20242e;color:#fff;border-radius:8px;font:13px system-ui;pointer-events:none';
  surface.append(canvas,editor,accessibility,commandStatus);target.append(surface);
  let id,storage,closed=false,composing=false,pasteShift=false,flush=Promise.resolve(),lastState,readyResolve,readyReject,persistenceFailed=false;
  const presented=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  void presented.catch(()=>{});
  const report=data=>{
    if(data.desktopCommand){const row=data.desktopCommand;commandStatus.style.display='block';commandStatus.textContent=row.state==='succeeded'?'Desktop change saved':row.state==='submitting'||row.state==='accepted'?'Saving desktop change…':row.state==='failed'||row.state==='rejected'?`Desktop change refused: ${row.error??'unavailable'}`:`Desktop change awaiting confirmation: ${row.error??row.state}`;}
    if(data.desktopErrors?.length){commandStatus.style.display='block';commandStatus.textContent=`Desktop request unavailable: ${data.desktopErrors.at(-1)}`;}
    options.onStatus?.(data);
  };
  const presentationWaiters=new Set();
  const config={app:'writer',profile:'renderer-proof',device:'browser-proof-device',room:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',author:'Browser proof user',instance:1,mac:/Mac/.test(navigator.platform),...options};
  delete config.onNavigation;delete config.onStatus;delete config.signal;delete config.loadAsset;delete config.actionsV1;
  let semanticKeys,actionPalette,desktopCommands,navigation;
  const semantic=semanticBridge(accessibility,(...args)=>runtime.semantic(id,...args),report,{
    keydown:(...args)=>active()&&semanticKeys?.keydown(...args),
    keyup:(...args)=>active()&&semanticKeys?.keyup(...args),
    blur:owner=>{if(active())semanticKeys?.blur(owner);},
  });
  const listeners=new AbortController();const on=(node,type,fn,opts={})=>node.addEventListener(type,fn,{...opts,signal:listeners.signal});
  function active(){return !closed&&!surface.inert&&registry.current(token)&&id!==undefined;}
  function update(raw){
    if(closed||!registry.current(token))return;
    navigation?.refresh();
    if(desktopCommands)void desktopCommands.pump();
    if(actionPalette){
      const request=runtime.take_caller_action(id);
      if(request)void actionPalette.submit(JSON.parse(request)).catch(error=>{if(active())report({actionError:String(error)});});
    }
    const state=JSON.parse(raw);lastState={...lastState,...state};report(state);
    if(state.phase==='suspended'){surface.inert=true;editor.disabled=true;}
    if(state.phase==='presented'){surface.inert=false;editor.disabled=false;}
    surface.dataset.phase=state.phase;surface.dataset.frames=String(state.frames??0);
    if(state.phase==='failed'){readyReject(Error(state.error));for(const waiter of presentationWaiters)waiter.reject(Error(state.error));presentationWaiters.clear();}
    if(state.phase==='presented'){for(const waiter of [...presentationWaiters])if((state.frames??0)>waiter.after){presentationWaiters.delete(waiter);waiter.resolve();}}
    if(state.semantics)semantic.update(state.semantics);
    if(state.ime){
      editor.style.left=`${state.ime.x}px`;editor.style.top=`${state.ime.y}px`;editor.style.height=`${Math.max(1,state.ime.height)}px`;
      if(document.activeElement===canvas)editor.focus({preventScroll:true});
    }else if(document.activeElement===editor){canvas.focus({preventScroll:true});}
    if(state.phase==='presented'){readyResolve();void persist().catch(()=>{});void loadAssets();}
  }
  const assets=createAssetRecovery({active,report,
    retry:request=>runtime.retry_asset(id,request.loader,request.url),
    complete:(request,bytes)=>runtime.complete_asset(id,request.loader,request.url,bytes),
    load:async request=>{
      if(options.loadAsset)return options.loadAsset(request.url,listeners.signal);
      const url=new URL(request.url,location.href);
      if(url.origin!==location.origin||!['http:','https:'].includes(url.protocol))throw Error('asset unavailable: no admitted content transport');
      const response=await fetch(url,{signal:listeners.signal,credentials:'same-origin'});
      if(!response.ok)throw Error(`asset HTTP ${response.status}`);
      return new Uint8Array(await response.arrayBuffer());
    }
  });
  async function loadAssets(){
    if(!active())return;
    await Promise.allSettled(JSON.parse(runtime.asset_requests(id)).map(request=>assets.request(request)));
  }
  async function retryAsset(assetId){
    if(!active())throw Error('Asset surface disposed');
    await assets.recover(assetId);
    if(!active())throw Error('Asset surface disposed');
    // Frames shown while fetching cannot prove presentation of the decoded asset.
    const after=lastState?.frames??0;
    await new Promise((resolve,reject)=>{
      const timeout=setTimeout(()=>{presentationWaiters.delete(waiter);reject(Error('Asset presentation timed out'));},10000);
      const waiter={after,resolve:()=>{clearTimeout(timeout);resolve();},reject:error=>{clearTimeout(timeout);reject(error);}};presentationWaiters.add(waiter);
    });
  }
  async function persist(){
    if(!active()||!storage||persistenceFailed)return;
    const current=id;
    flush=flush.catch(()=>{}).then(async()=>{
      if(!active()||persistenceFailed)return;
      const writes=JSON.parse(runtime.pending_writes(current));
      if(!writes.length)return;
      try{
        if(config.peer!==undefined&&(runtime.peer_pending_batch_bytes||runtime.peer_pending_batch)){
          await commitPeerBatches({pending:()=>readPeerBatch(runtime,config.peer),commit:batch=>storage.commit(batch),acknowledge:result=>runtime.acknowledge_peer_storage_batch(config.peer,JSON.stringify(result)),epoch:storage.epoch,publish:()=>{}});
        }else{
          await storage.commit(writes);
          for(const w of writes)runtime.acknowledge_storage(config.profile,config.device,BigInt(w.revision),undefined);
        }
        report({persistence:'committed',pendingWrites:0,error:null});
      }catch(error){
        for(const w of writes)runtime.acknowledge_storage(config.profile,config.device,BigInt(w.revision),String(error));
        persistenceFailed=true;report({persistence:'failed',pendingWrites:JSON.parse(runtime.pending_writes(current)).length,error:String(error)});throw error;
      }
    });
    return flush;
  }
  let closeWrites=[];
  const cleanup=recoverableClose({
    begin(){
      navigation?.close();actionPalette?.close();desktopCommands?.close();semantic.close();closed=true;for(const waiter of presentationWaiters)waiter.reject(Error('mount disposed'));presentationWaiters.clear();listeners.abort();readyReject(Error('mount disposed'));
      if(id!==undefined)closeWrites=JSON.parse(runtime.close(id));
      surface.remove();
    },
    async settle(){
      await flush.catch(()=>{});
      if(storage&&id!==undefined)await settleComponentClose(runtime,id,storage,config.profile,config.device);
      if(storage){await storage.release();storage=undefined;}
      recoveryJobs.delete(cleanup);closures.delete(cleanup);
      report({phase:'closed',persistence:'committed',pendingWrites:0,error:null});
    },
    onError(error){
      recoveryJobs.add(cleanup);
      report({phase:'closed',persistence:'failed',pendingWrites:closeWrites.length,recoveryAvailable:true,error:String(error)});
    }
  });
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
    const publishModifiers=event=>runtime.modifiers(id,...modifierSnapshot(event));
    const clearModifiers=()=>runtime.modifiers(id,false,false,false,false);
    for(const node of [canvas,editor]){
      on(node,'keydown',event=>{
        if(active())publishModifiers(event);
        if(!composing&&(event.ctrlKey||event.metaKey)&&['v','c','x'].includes(event.key.toLowerCase())){pasteShift=event.shiftKey;event.stopImmediatePropagation();}
      },{capture:true});
      on(node,'keyup',event=>{if(active())publishModifiers(event);},{capture:true});
      for(const kind of ['paste','copy','cut'])on(node,kind,event=>{
        if(!active()||!event.clipboardData||composing)return;
        try{if(kind==='paste'&&!event.clipboardData.getData('text/plain')){event.preventDefault();report({clipboard:'empty or unsupported clipboard payload'});return;}const completed=runtime.clipboard_event(id,kind,event.clipboardData,pasteShift);event.preventDefault();report({lastInput:kind,clipboard:completed?'completed in trusted DOM event':'unavailable or no selection'});}
        catch(error){report({clipboard:'failed',error:String(error)});}
      });
    }
    const dispatchKey=(type,event)=>canvas.dispatchEvent(new KeyboardEvent(type,event));
    const hiddenKeys=createKeyboardForwarder({route:keyboardRoute,mac:config.mac,dispatch:dispatchKey,clearModifiers,report:()=>report({lastInput:'keyboard'})});
    semanticKeys=createKeyboardForwarder({route:(...args)=>overlayKeyboardRoute(lastState?.semantics?.root?.widget,...args),mac:config.mac,dispatch:dispatchKey,publishModifiers,clearModifiers,report:()=>report({lastInput:'keyboard'})});
    const hiddenKeyOwner={};
    const dispatchCanvasKey=key=>{canvas.dispatchEvent(new KeyboardEvent('keydown',{key}));canvas.dispatchEvent(new KeyboardEvent('keyup',{key}));};
    on(editor,'keydown',event=>{
      hiddenKeys.keydown(hiddenKeyOwner,event,composing);
    });
    on(editor,'keyup',event=>hiddenKeys.keyup(hiddenKeyOwner,event));
    on(editor,'compositionstart',()=>{composing=true;runtime.ime(id,'start','');report({lastInput:'compositionstart'});});
    on(editor,'compositionupdate',event=>{runtime.ime(id,'preedit',event.data);});
    on(editor,'compositionend',event=>{runtime.ime(id,'preedit','');runtime.ime(id,'commit',event.data);composing=false;editor.value='';report({lastInput:'compositioncommit'});});
    on(editor,'beforeinput',event=>{const route=beforeInputRoute(event,composing);if(route.kind==='ignore')return;event.preventDefault();if(route.kind==='commit'){runtime.ime(id,'commit',route.text);editor.value='';report({lastInput:'beforeinput'});}else if(route.kind==='key'){dispatchCanvasKey(route.key);report({lastInput:'beforeinput-key'});}});
    on(editor,'focus',()=>{canvas.dispatchEvent(new FocusEvent('focus'));if(active())runtime.ime(id,'start','');});
    on(editor,'blur',()=>{if(active()){if(composing){runtime.ime(id,'preedit','');composing=false;report({lastInput:'compositioncancel'});}hiddenKeys.blur(hiddenKeyOwner);runtime.ime(id,'end','');canvas.dispatchEvent(new FocusEvent('blur'));}});
    on(canvas,'blur',()=>{if(active())clearModifiers();});
    on(canvas,'pointerdown',()=>report({lastInput:'pointer'}));
    on(canvas,'wheel',event=>{
      if(event.deltaMode===2){event.preventDefault();const [x,y]=wheelPixels(event.deltaX,event.deltaY,2,canvas.clientWidth,canvas.clientHeight);canvas.dispatchEvent(new WheelEvent('wheel',{deltaX:-x,deltaY:-y,deltaMode:0,ctrlKey:event.ctrlKey,shiftKey:event.shiftKey}));}
      report({lastInput:'wheel'});
    },{passive:false});
    on(document,'visibilitychange',()=>{if(active())runtime.visible(id,document.visibilityState==='visible');});
    await presented;
    if(!active())throw Error('mount disposed');
    if(options.actionsV1)actionPalette=bindActionPalette({actions:options.actionsV1,publish:rows=>{if(active())runtime.caller_actions(id,JSON.stringify(rows));},report});
    if(options.actionsV1)desktopCommands=bindDesktopCommands({actions:options.actionsV1,peek:()=>{if(!active())return null;const raw=runtime.pending_desktop_command(id);return raw?JSON.parse(raw):null;},ack:invocation=>{if(active())runtime.acknowledge_desktop_command(id,invocation);},report});
    if(runtime.desktop_navigation_snapshot && runtime.desktop_navigation_snapshot(id)) {
      navigation=createDesktopNavigation({active,snapshot:()=>runtime.desktop_navigation_snapshot(id),revision:()=>runtime.desktop_navigation_revision(id),dispatch:command=>runtime.desktop_navigate(id,command),search:(generation,query)=>runtime.desktop_command_search(id,generation,query),globalSearch:runtime.desktop_global_search?(generation,query)=>runtime.desktop_global_search(id,generation,query):undefined,focusSearch:()=>canvas.focus({preventScroll:true}),awaitTerminal:desktopCommands?invocation=>desktopCommands.waitForTerminal(invocation):undefined});
      // Cloud's toolbar is a sibling of the renderer in its Desktop section.
      // Scope the listener to that mounted desktop; other page editors stay native.
      const shortcutScope=target.closest('section[aria-label="Desktop"]')??target;
      on(shortcutScope,'keydown',event=>{
        if(!active()||(!surface.contains(event.target)&&event.target.closest?.('input,textarea,[contenteditable="true"],[role="textbox"]')))return;
        routeGlobalSearchShortcut(event,composing,()=>navigation.openGlobalSearch());
      },{capture:true});
      options.onNavigation?.(navigation.controller);
    }
    return {navigation: navigation?.controller,retryAsset,cleanup:async()=>{registry.remove(token);await cleanup();},flush:()=>{persistenceFailed=false;return persist();},
      state:()=>lastState,canvas,editor,semantics:accessibility,
      durableState:()=>acquired.hydrate(),probePixels:async()=>JSON.parse(await runtime.probe_pixels(id)),failNextWrite:()=>{storage.abortNext=true;},destroyDevice:()=>runtime.destroy_device(id),recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args)};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
export function pendingComponentCloseCount(){return runtime.component_close_count();}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
