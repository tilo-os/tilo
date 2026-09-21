import {createDesktopNavigation} from './desktop-navigation.mjs';
import {createNavigationFocus} from './desktop-navigation-focus.mjs';
import {bindActionPalette} from './action-palette-bridge.mjs';
import {bindDesktopCommands} from './desktop-command-bridge.mjs';
import {commitPeerBatches,readPeerBatch} from './peer-persistence.mjs';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
import {createFontDemandPump,createFontLoader} from './font-loader.mjs';
import init,* as runtime from './pkg/qos_browser_runtime.js';
import {MountRegistry,createInputScope,createFocusHandoff,createCompositionSession,beforeInputRoute,createKeyboardForwarder,keyboardRoute,modifierSnapshot,overlayKeyboardRoute,retainStatus,retainedWidget,routeGlobalSearchShortcut,wheelPixels} from './host-core.mjs';

import {acquireStorage} from './storage.mjs';
import {semanticBridge} from './semantics.mjs';
import {settleComponentClose,recoverableClose} from './close-recovery.mjs';
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
const fontLoader=createFontLoader({baseURL:import.meta.url});
const fonts=retryableMemo(()=>fontLoader.startup());
const ready=retryableMemo(()=>init());
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='qos-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;';
  // Native focus may scroll hidden semantic controls; it must not move the canvas viewport.
  const surface=document.createElement('div');surface.className='qos-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:clip';
  let editor=document.createElement('textarea');editor.setAttribute('aria-label','qOS focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none';
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','qOS application');accessibility.style.cssText='position:absolute;top:0;left:0;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);';
  const commandStatus=document.createElement('div');commandStatus.setAttribute('role','status');commandStatus.setAttribute('aria-live','polite');
  commandStatus.style.cssText='display:none;position:absolute;bottom:12px;left:12px;max-width:80%;padding:8px 12px;background:#20242e;color:#fff;border-radius:8px;font:13px system-ui;pointer-events:none';
  surface.append(canvas,editor,accessibility,commandStatus);target.append(surface);
  let id,storage,closed=false,composing=false,pasteShift=false,flush=Promise.resolve(),lastState,readyResolve,readyReject,persistenceFailed=false;
  const presented=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  void presented.catch(()=>{});
  const report=data=>{
    if(data.clipboard==='copied'||data.clipboard==='failed'){commandStatus.style.display='block';commandStatus.textContent=data.clipboard==='copied'?'Copied to system clipboard':`Could not copy to system clipboard: ${data.clipboardError??'browser access denied'}`;}
    if(data.desktopCommand){const row=data.desktopCommand;commandStatus.style.display='block';commandStatus.textContent=row.state==='succeeded'?'Desktop change saved':row.state==='submitting'||row.state==='accepted'?'Saving desktop change…':row.state==='failed'||row.state==='rejected'?`Desktop change refused: ${row.error??'unavailable'}`:`Desktop change awaiting confirmation: ${row.error??row.state}`;}
    if(data.desktopErrors?.length){commandStatus.style.display='block';commandStatus.textContent=`Desktop request unavailable: ${data.desktopErrors.at(-1)}`;}
    options.onStatus?.(data);
  };
  const navigationFocus=createNavigationFocus({document,surface,canvas,active});
  const presentationWaiters=new Set();
  const config={app:'writer',profile:'renderer-proof',device:'browser-proof-device',room:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',author:'Browser proof user',instance:1,mac:/Mac/.test(navigator.platform),...options};
  delete config.onNavigation;delete config.onStatus;delete config.signal;delete config.loadAsset;delete config.actionsV1;delete config.subscribeDesktopCommits;
  let unsubscribeDesktopCommits;
  let semantic,semanticKeys,hiddenKeys,composition,actionPalette,desktopCommands,navigation,fontDemands,externalFocusEpoch=0,runtimeReady=false,desktopOwnership=false,localActivationGesture;
  const inputScope=createInputScope({active,read:()=>desktopOwnership?runtime.desktop_input_ownership(id):undefined,onChange:()=>{
    hiddenKeys?.invalidate();semanticKeys?.invalidate();composition?.cancel();semantic?.revokeInput();composing=false;editor.value='';pasteShift=false;
    replaceEditor();
  }});
  const handoff=createFocusHandoff({scope:inputScope,epoch:()=>externalFocusEpoch,focus:state=>{
    (state.ime?editor:canvas).focus({preventScroll:true});
  }});
  function observeLocalActivation(){
    const gesture=localActivationGesture;
    if(!gesture)return;
    if(!active()||gesture.epoch!==externalFocusEpoch){localActivationGesture=undefined;return;}
    const current=inputScope.capture();
    if(current&&(current.sequence!==gesture.scope.sequence||current.activation!==gesture.scope.activation||current.owner!==gesture.scope.owner)){
      localActivationGesture=undefined;void handoff.complete(gesture.scope,gesture.epoch);
    }
  }
  semantic=semanticBridge(accessibility,(...args)=>runtime.semantic(id,...args),report,{
    keydown:(...args)=>{inputScope.capture();return active()&&semanticKeys?.keydown(...args);},
    keyup:(...args)=>{inputScope.capture();return active()&&semanticKeys?.keyup(...args);},
    blur:owner=>{inputScope.capture();if(active())semanticKeys?.blur(owner);},
  },inputScope);
  const listeners=new AbortController();const on=(node,type,fn,opts={})=>node.addEventListener(type,fn,{...opts,signal:listeners.signal});
  const editorListeners=[];
  function onEditor(type,fn,opts={}){
    const guarded=event=>{inputScope.capture();if(event.currentTarget!==editor){event.preventDefault();return;}fn(event);};
    editorListeners.push([type,guarded,opts]);on(editor,type,guarded,opts);
  }
  function newComposition(){return createCompositionSession({scope:inputScope,send:(kind,text)=>runtime.ime(id,kind,text),clear:()=>{editor.value='';composing=false;}});}
  function replaceEditor(restoreFocus=true){
    const old=editor,focused=restoreFocus&&document.activeElement===old;
    editor=old.cloneNode(false);editor.value='';
    for(const [type,fn,opts] of editorListeners)on(editor,type,fn,opts);
    composition=newComposition();old.replaceWith(editor);
    if(focused){const scope=inputScope.capture();if(scope)(scope.legacy||scope.ime?editor:canvas).focus({preventScroll:true});}
  }
  function active(){return !closed&&!surface.inert&&registry.current(token)&&id!==undefined&&runtimeReady;}
  const fontMountValid=()=>!closed&&registry.current(token)&&id!==undefined&&runtimeReady;
  fontDemands=createFontDemandPump({loader:fontLoader,valid:fontMountValid,report,install:(name,bytes)=>runtime.install_font(id,name,bytes),failed:()=>runtime.font_failed(id)});
  function update(raw){
    if(closed||!registry.current(token))return;
    const state=JSON.parse(raw);if(state.clipboard){report(state);return;}
    if(!runtimeReady&&['gpu-pending','presented'].includes(state.phase)){
      desktopOwnership=Boolean(runtime.desktop_input_ownership&&runtime.desktop_navigation_snapshot?.(id));
      runtimeReady=true;
    }
    inputScope.capture();
    if(runtimeReady&&runtime.font_demands){
      const demanded=JSON.parse(runtime.font_demands(id));
      if(demanded.length)void fontDemands(demanded);
    }
    navigation?.refresh();

    if(desktopCommands)void desktopCommands.pump();
    if(actionPalette){
      const request=runtime.take_caller_action(id);
      if(request)void actionPalette.submit(JSON.parse(request)).catch(error=>{if(active())report({actionError:String(error)});});
    }
    lastState=retainStatus(lastState,state);report(state);
    if(state.phase==='suspended'){surface.inert=true;editor.disabled=true;inputScope.capture();}

    if(state.phase==='presented'){surface.inert=false;editor.disabled=false;}
    surface.dataset.phase=state.phase;surface.dataset.frames=String(state.frames??0);
    if(state.phase==='failed'){readyReject(Error(state.error));for(const waiter of presentationWaiters)waiter.reject(Error(state.error));presentationWaiters.clear();}
    if(state.phase==='presented'){for(const waiter of [...presentationWaiters])if((state.frames??0)>waiter.after){presentationWaiters.delete(waiter);waiter.resolve();}}
    navigation?.refresh();
    navigationFocus.errors(state.desktopErrors??[]);
    if(state.phase==='presented'&&navigation)navigationFocus.update(navigation.controller.getSnapshot());
    if(state.semantics)semantic.update(state.semantics);
    const ownership=inputScope.capture(),ime=ownership?.legacy?state.ime:ownership?.ime;
    if(ime){
      editor.style.left=`${ime.x}px`;editor.style.top=`${ime.y}px`;editor.style.height=`${Math.max(1,ime.height)}px`;
      if(document.activeElement===canvas)editor.focus({preventScroll:true});
    }else if(document.activeElement===editor){canvas.focus({preventScroll:true});}
    observeLocalActivation();
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
      unsubscribeDesktopCommits?.();navigationFocus.close();navigation?.close();actionPalette?.close();desktopCommands?.close();semantic.close();closed=true;for(const waiter of presentationWaiters)waiter.reject(Error('mount disposed'));presentationWaiters.clear();listeners.abort();readyReject(Error('mount disposed'));
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
    inputScope.capture();
    const publishModifiers=event=>{if(inputScope.capture())runtime.modifiers(id,...modifierSnapshot(event));};
    const clearModifiers=()=>{if(inputScope.capture())runtime.modifiers(id,false,false,false,false);};
    for(const type of ['pointerdown','focusin','keydown'])on(document,type,event=>{if(!surface.contains(event.target))externalFocusEpoch++;},{capture:true});
    // Search accepts results inside Rust. Only a local surface gesture may
    // authorize that activation. A later status may complete the gesture, but
    // cannot create one or outlive a subsequent outside-focus gesture.
    for(const type of ['pointerdown','pointerup','click','keydown','keyup','input'])on(surface,type,()=>{
      const scope=inputScope.capture();
      if(scope&&(['pointerdown','keydown'].includes(type)||!localActivationGesture))localActivationGesture={scope,epoch:externalFocusEpoch};
      queueMicrotask(observeLocalActivation);
    },{capture:true});
    for(const node of [canvas,editor]){
      const bind=node===editor?onEditor:(type,fn,opts)=>on(node,type,fn,opts);
      bind('keydown',event=>{
        inputScope.capture();
        if(active())publishModifiers(event);
        if(!composing&&(event.ctrlKey||event.metaKey)&&['v','c','x'].includes(event.key.toLowerCase())){pasteShift=event.shiftKey;event.stopImmediatePropagation();}
      },{capture:true});
      bind('keyup',event=>{if(active())publishModifiers(event);},{capture:true});
      for(const kind of ['paste','copy','cut'])bind(kind,event=>{
        const scope=inputScope.capture();
        if(!scope||!event.clipboardData||composing||!inputScope.valid(scope))return;
        try{if(kind==='paste'&&!event.clipboardData.getData('text/plain')){event.preventDefault();report({clipboard:'empty or unsupported clipboard payload'});return;}const completed=runtime.clipboard_event(id,kind,event.clipboardData,pasteShift);event.preventDefault();report({lastInput:kind,clipboard:completed?'completed in trusted DOM event':'unavailable or no selection'});}
        catch(error){report({clipboard:'failed',error:String(error)});}
      });
    }
    const dispatchKey=(type,event)=>canvas.dispatchEvent(new KeyboardEvent(type,event));
    hiddenKeys=createKeyboardForwarder({route:keyboardRoute,mac:config.mac,dispatch:dispatchKey,clearModifiers,report:()=>report({lastInput:'keyboard'})});
    semanticKeys=createKeyboardForwarder({route:(...args)=>overlayKeyboardRoute(retainedWidget(lastState),...args),mac:config.mac,dispatch:dispatchKey,publishModifiers,clearModifiers,report:()=>report({lastInput:'keyboard'})});

    const hiddenKeyOwner={};
    const dispatchCanvasKey=key=>{canvas.dispatchEvent(new KeyboardEvent('keydown',{key}));canvas.dispatchEvent(new KeyboardEvent('keyup',{key}));};
    // Canvas clicks are hit-tested by the runtime. Page chrome is outside that
    // surface, so route dismissal to the current search owner without stealing
    // focus or consuming the external control's pointer event.
    on(document,'pointerdown',event=>{
      if(!active()||surface.contains(event.target))return;
      if(inputScope.capture()?.owner==='system-search')dispatchCanvasKey('Escape');
    },{capture:true});
    onEditor('keydown',event=>{
      inputScope.capture();if(!active())return;
      hiddenKeys.keydown(hiddenKeyOwner,event,composing);
    });
    onEditor('keyup',event=>{inputScope.capture();if(active())hiddenKeys.keyup(hiddenKeyOwner,event);});
    composition=newComposition();
    onEditor('compositionstart',()=>{composing=composition.start();if(composing)report({lastInput:'compositionstart'});});
    onEditor('compositionupdate',event=>composition.update(event.data));
    onEditor('compositionend',event=>{if(composition.end(event.data))report({lastInput:'compositioncommit'});composing=false;if(event.currentTarget===editor)replaceEditor();});
    onEditor('beforeinput',event=>{
      const scope=inputScope.capture();
      if(scope&&composition.composing)return;
      if(!scope||composition.blocks(event)){event.preventDefault();return;}
      const route=beforeInputRoute(event,composing);if(route.kind==='ignore')return;event.preventDefault();
      if(!inputScope.valid(scope))return;
      if(route.kind==='commit'){runtime.ime(id,'commit',route.text);editor.value='';report({lastInput:'beforeinput'});}
      else if(route.kind==='key'){dispatchCanvasKey(route.key);report({lastInput:'beforeinput-key'});}
    });
    onEditor('input',()=>{if(!composition.composing)editor.value='';});
    let editorFocusScope,canvasFocusScope;
    onEditor('focus',()=>{editorFocusScope=inputScope.capture();if(!editorFocusScope)return;canvas.dispatchEvent(new FocusEvent('focus'));runtime.ime(id,'start','');});
    onEditor('blur',()=>{
      if(!inputScope.valid(editorFocusScope))return;
      if(composition.composing){runtime.ime(id,'preedit','');composition.cancel();composing=false;replaceEditor(false);report({lastInput:'compositioncancel'});}
      hiddenKeys.blur(hiddenKeyOwner);runtime.ime(id,'end','');canvas.dispatchEvent(new FocusEvent('blur'));
    });
    on(canvas,'focus',()=>{canvasFocusScope=inputScope.capture();});
    on(canvas,'blur',()=>{if(inputScope.valid(canvasFocusScope))clearModifiers();});
    // Throttled statuses omit the tree; assistive technology entering the surface gets the complete current tree at once.
    const requestSemantics=()=>{if(active())runtime.request_semantics?.(id);};
    on(accessibility,'focusin',event=>{if(!accessibility.contains(event.relatedTarget))requestSemantics();});

    on(canvas,'pointerdown',()=>report({lastInput:'pointer'}));
    on(canvas,'wheel',event=>{
      if(event.deltaMode===2){event.preventDefault();const [x,y]=wheelPixels(event.deltaX,event.deltaY,2,canvas.clientWidth,canvas.clientHeight);canvas.dispatchEvent(new WheelEvent('wheel',{deltaX:-x,deltaY:-y,deltaMode:0,ctrlKey:event.ctrlKey,shiftKey:event.shiftKey}));}
      report({lastInput:'wheel'});
    },{passive:false});
    on(document,'visibilitychange',()=>{if(active())runtime.visible(id,document.visibilityState==='visible');});
    await presented;
    if(!active())throw Error('mount disposed');
    if(options.actionsV1)actionPalette=bindActionPalette({actions:options.actionsV1,publish:rows=>{if(active())runtime.caller_actions(id,JSON.stringify(rows));},report});
    if(options.actionsV1)desktopCommands=bindDesktopCommands({actions:options.actionsV1,onPending:pending=>navigation?.commandPending(pending),onUpdate:row=>navigation?.commandUpdated(row),peek:()=>{if(!active())return null;const raw=runtime.pending_desktop_command(id);return raw?JSON.parse(raw):null;},ack:invocation=>{if(active())runtime.acknowledge_desktop_command(id,invocation);},report});
    if(runtime.desktop_navigation_snapshot && runtime.desktop_navigation_snapshot(id)) {
      navigation=createDesktopNavigation({active,captureLaunchFocus:()=>({activation:inputScope.capture()?.activation,epoch:externalFocusEpoch}),snapshot:()=>runtime.desktop_navigation_snapshot(id),revision:()=>runtime.desktop_navigation_revision(id),dispatch:command=>{
        const request=navigationFocus.begin(JSON.parse(command),navigation.controller.getSnapshot());
        try{const result=runtime.desktop_navigate(id,command);navigationFocus.accepted(request);return result;}
        catch(error){navigationFocus.failed(request);throw error;}
      },search:(generation,query)=>runtime.desktop_command_search(id,generation,query),globalSearch:runtime.desktop_global_search?(generation,query)=>runtime.desktop_global_search(id,generation,query):undefined,focusSearch:()=>{inputScope.capture();canvas.focus({preventScroll:true});},captureSelect:()=>({scope:inputScope.capture(),epoch:externalFocusEpoch}),completeSelect:before=>handoff.complete(before.scope,before.epoch),awaitTerminal:desktopCommands?invocation=>desktopCommands.waitForTerminal(invocation):undefined});
      unsubscribeDesktopCommits=options.subscribeDesktopCommits?.(event=>{if(active())navigation.commandCommitted(event);});

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
      state:()=>lastState,canvas,get editor(){return editor;},semantics:accessibility,requestSemantics,

      durableState:()=>acquired.hydrate(),probePixels:async()=>JSON.parse(await runtime.probe_pixels(id)),failNextWrite:()=>{storage.abortNext=true;},destroyDevice:()=>runtime.destroy_device(id),recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args)};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
export function pendingComponentCloseCount(){return runtime.component_close_count();}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
