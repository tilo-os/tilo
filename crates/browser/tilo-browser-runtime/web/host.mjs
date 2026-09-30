import {createDesktopNavigation} from './desktop-navigation.mjs';
import {createNavigationFocus} from './desktop-navigation-focus.mjs';
import {bindActionPalette} from './action-palette-bridge.mjs';
import {bindDesktopCommands} from './desktop-command-bridge.mjs';
import {createDesktopSpeculation} from './desktop-speculation.mjs';
import {commitPeerBatches,readPeerBatch} from './peer-persistence.mjs';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
import {createFontDemandPump,createFontLoader,createIdleFontInstaller,createIdlePrewarm} from './font-loader.mjs';
import {MountRegistry,reopenedRecord,createInputScope,createFocusHandoff,createCompositionSession,beforeInputRoute,MAX_TEXT_INSERT,oversizedInsert,createKeyboardForwarder,keyboardRoute,modifierSnapshot,overlayKeyboardRoute,retainStatus,retainedWidget,routeGlobalSearchShortcut,wheelPixels} from './host-core.mjs';

import {acquireStorage} from './storage.mjs';
import {semanticBridge} from './semantics.mjs';
import {createLiveOverlay} from './live-overlay.mjs';
import {createLegacyWindows} from './legacy-windows.mjs';
import {settleComponentClose,recoverableClose} from './close-recovery.mjs';
import {SideModuleLinker} from './dylink.mjs';
import {createAppInstaller,canonicalAppId,localSeed} from './app-installer.mjs';
import {importGlue,loadCore} from './core-loader.mjs';
import {checkOpens,loadOpenBytes,openRequest} from './open-file.mjs';
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
const fontLoader=createFontLoader({baseURL:import.meta.url});
const fonts=retryableMemo(()=>fontLoader.startup());
// After the first presented frame, idle time fetches and verifies optional
// fonts an app open may demand (emoji and symbol glyphs first), so opening an
// app installs them from memory instead of the network. Bounded to 8 MiB.
// The preferred ones (what Chat and Writer chrome demand) are then installed
// into each live mount in idle slices, so an open does not install on its
// first frames either.
const APP_FONTS=['Noto-COLRv1.ttf','NotoSansSymbols[wght].ttf','NotoSansSymbols2-Regular.ttf','NotoSansMath-Regular.ttf'];
const prewarmFonts=createIdlePrewarm(()=>fontLoader.prewarm({prefer:APP_FONTS}));
// The core: by default the glue and module next to this file; a page may
// supply a verified one instead (configureRuntime) before its first mount.
let runtime,core,appLinker,supplied;
const ready=retryableMemo(async()=>{
  const source=supplied??{glue:import('./pkg/tilo_browser_runtime.js'),module:new URL('./pkg/tilo_browser_runtime_bg.wasm',import.meta.url)};
  core=await loadCore({glue:await importGlue(await source.glue),module:source.module});
  runtime=core.runtime;
  return core;
});
/**
 * Run a core from a verified source instead of `./pkg/`: `glue` is the
 * self-contained glue (its module namespace, or a URL such as the object URL
 * of a verified asset), `module` the core module (WebAssembly.Module,
 * Response or bytes). The registry client's `resolveRuntime()` supplies both.
 * Must precede the first mount; throws once a core is loading.
 */
export function configureRuntime({glue,module}){
  if(core||supplied)throw Error('the core is already configured');
  if(!glue||!module)throw TypeError('configureRuntime needs {glue, module}');
  supplied={glue,module};
}
/** The loaded core's runtime ABI id, once it has loaded. */
export async function runtimeAbiId(){return (await ready()).abi;}
// Apps the core does not contain arrive as side modules (WEB_RUNTIME.md).
// A core built without `dylink` has no linker, so only its compiled apps exist.
const apps=createAppInstaller({
  ready,
  installed:app=>{try{runtime.application(app);return true;}catch{return false;}},
  canonical:canonicalAppId,
  linker:()=>appLinker??=new SideModuleLinker(core.exports,core.abi),
});
/**
 * The resolver mounts use when they pass none: `(appId, {signal}) =>
 * {module, abi, assets}`, such as the registry client's `resolveApp`.
 */
export function setAppResolver(resolver){apps.setResolver(resolver);}
/**
 * Resolve and link apps before they are mounted, concurrently with each other
 * and with the core's own download (a session restore passes every app it
 * will open). Resolves to their ids; rejects with an AggregateError naming
 * each app that could not be installed in `failed`.
 */
export function preloadApps(appIds,options={}){return apps.preload(appIds,options);}
/** The static assets of a linked app: asset name to URL. */
export function appAssets(appId){return apps.assets(appId);}
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='tilo-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;';
  // Native focus may scroll hidden semantic controls; it must not move the canvas viewport.
  const surface=document.createElement('div');surface.className='tilo-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:clip';
  let editor=document.createElement('textarea');editor.setAttribute('aria-label','Tilo focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none';
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','Tilo application');accessibility.style.cssText='position:absolute;top:0;left:0;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);';
  const commandStatus=document.createElement('div');commandStatus.setAttribute('role','status');commandStatus.setAttribute('aria-live','polite');
  commandStatus.style.cssText='display:none;position:absolute;bottom:12px;left:12px;max-width:80%;padding:8px 12px;background:#20242e;color:#fff;border-radius:8px;font:13px system-ui;pointer-events:none';
  surface.append(canvas,editor,accessibility,commandStatus);target.append(surface);
  let id,application,storage,closed=false,composing=false,pasteShift=false,flush=Promise.resolve(),lastState,opened,readyResolve,readyReject,persistenceFailed=false;
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
  delete config.onNavigation;delete config.onStatus;delete config.signal;delete config.loadAsset;delete config.actionsV1;delete config.subscribeDesktopCommits;delete config.live;delete config.liveMembers;delete config.resolveApp;delete config.open;
  // A local desktop names apps by id or alias; the core resolves exact ids.
  const local=config.app==='desktop'&&options.local!==undefined?localSeed(options.local):undefined;
  if(local)config.local=local.seed;else delete config.local;
  let unsubscribeDesktopCommits;
  let liveOverlay;
  let legacyWindows;
  let semantic,semanticKeys,hiddenKeys,composition,actionPalette,desktopCommands,desktopSpeculation,navigation,fontDemands,externalFocusEpoch=0,runtimeReady=false,desktopOwnership=false,localActivationGesture;
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
  const fontInstaller=createIdleFontInstaller({loader:fontLoader,names:APP_FONTS,valid:fontMountValid,install:(name,bytes)=>runtime.install_font(id,name,bytes)});
  function update(raw){
    if(closed||!registry.current(token))return;
    const state=JSON.parse(raw);if(state.clipboard){report(state);return;}
    if(state.opened)opened=state.opened;
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

    if(state.phase==='presented'){surface.inert=false;editor.disabled=false;liveOverlay?.wake();legacyWindows?.wake();}
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
    if(state.phase==='presented'){readyResolve();void persist().catch(()=>{});void loadAssets();void prewarmFonts().then(()=>{if(fontMountValid())fontInstaller.start();});}
  }
  // The one path bytes enter the page by: the page's own loader, an app's
  // static asset where its resolver placed it, or a same-origin fetch.
  async function admittedBytes(requested){
    if(options.loadAsset)return options.loadAsset(requested,listeners.signal);
    // A desktop's windows are any linked apps.
    const appAsset=application?apps.assets(application.id)?.get(requested):apps.asset(requested);
    const url=new URL(appAsset??requested,location.href);
    if((!appAsset&&url.origin!==location.origin)||!['http:','https:'].includes(url.protocol))throw Error('asset unavailable: no admitted content transport');
    const response=await fetch(url,{signal:listeners.signal,credentials:'same-origin'});
    if(!response.ok)throw Error(`asset HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  }
  const assets=createAssetRecovery({active,report,
    retry:request=>runtime.retry_asset(id,request.loader,request.url),
    complete:(request,bytes)=>runtime.complete_asset(id,request.loader,request.url,bytes),
    load:request=>admittedBytes(request.url)
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
  let busyRetryMs=0,busyRetry=null;
  function retryBusyPersist(){
    if(busyRetry!==null||!active())return;
    busyRetryMs=Math.min(1000,Math.max(25,busyRetryMs*2));
    busyRetry=setTimeout(()=>{busyRetry=null;void persist().catch(()=>{});},busyRetryMs);
  }
  async function persist(){
    if(!active()||!storage||persistenceFailed)return;
    const current=id;
    flush=flush.catch(()=>{}).then(async()=>{
      if(!active()||persistenceFailed)return;
      const writes=JSON.parse(runtime.pending_writes(current));
      if(!writes.length)return;
      try{
        if(config.peer!==undefined){
          const result=await commitPeerBatches({pending:()=>readPeerBatch(runtime,config.peer),commit:batch=>storage.commit(batch),acknowledge:result=>runtime.acknowledge_peer_storage_batch(config.peer,JSON.stringify(result)),epoch:storage.epoch,publish:()=>{}});
          // The peer session's delivery transaction holds the capture; retry once it settles.
          if(result.busy){retryBusyPersist();return;}
          busyRetryMs=0;
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
      if(busyRetry!==null){clearTimeout(busyRetry);busyRetry=null;}liveOverlay?.close();liveOverlay=undefined;legacyWindows?.close();legacyWindows=undefined;unsubscribeDesktopCommits?.();navigationFocus.close();navigation?.close();actionPalette?.close();desktopCommands?.close();desktopSpeculation?.close();semantic.close();closed=true;for(const waiter of presentationWaiters)waiter.reject(Error('mount disposed'));presentationWaiters.clear();listeners.abort();readyReject(Error('mount disposed'));
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
    if(!navigator.gpu)throw Error('WebGPU is required for the Tilo Vello compute renderer');
    // An app the core lacks is resolved while the core itself loads.
    const install={resolveApp:options.resolveApp,signal:options.signal,onStatus:report};
    const installing=config.app!=='desktop'?apps.ensure(config.app,install):local?.apps.length?apps.preload(local.apps,install):undefined;
    void installing?.catch(()=>{});
    const failed=error=>{report({phase:'failed',error:error instanceof Error?error.message:String(error)});return error instanceof Error?error:Error(String(error));};
    await ready();if(!registry.current(token))throw Error('mount disposed');
    // An app that is neither in the core nor resolvable rejects here, and so
    // does a file it does not open, before its bytes are fetched.
    let openBytes;
    try{
      await installing;application=config.app==='desktop'?undefined:JSON.parse(runtime.application(config.app));
      if(options.open!==undefined){
        const request=openRequest(options.open,location.href);
        checkOpens(application,request.mediaType);
        config.open={name:request.name,mediaType:request.mediaType};
        openBytes=loadOpenBytes(request,admittedBytes);
        void openBytes.catch(()=>{});
      }
    }catch(error){throw failed(error);}
    const acquired=await acquireStorage(config.profile,config.device);
    if(closed||!registry.current(token)){await acquired.release();throw Error("mount disposed");}
    storage=acquired;
    if(storage.fresh){runtime.refresh_storage_lease(config.profile,config.device);storage.fresh=false;}
    const [assets,entries,openFile]=await Promise.all([fonts(),storage.hydrate(),openBytes?.catch(error=>{throw failed(error);})]);
    if(!registry.current(token))throw Error('mount disposed');
    if(!config.record&&!config.newDocument)config.record=reopenedRecord(entries,application,config.room);
    id=runtime.mount(canvas,JSON.stringify(config),assets,entries,raw=>queueMicrotask(()=>update(raw)),openFile);
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
        try{if(kind==='paste'&&!event.clipboardData.getData('text/plain')){event.preventDefault();report({clipboard:'empty or unsupported clipboard payload'});return;}
          if(kind==='paste'&&oversizedInsert(event.clipboardData.getData('text/plain'))){event.preventDefault();report({clipboard:'refused',error:`Paste is too large (more than ${MAX_TEXT_INSERT.toLocaleString()} characters); nothing was inserted.`});return;}const completed=runtime.clipboard_event(id,kind,event.clipboardData,pasteShift);event.preventDefault();report({lastInput:kind,clipboard:completed?'completed in trusted DOM event':'unavailable or no selection'});}
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
      if(route.kind==='refuse'){editor.value='';report({lastInput:'beforeinput',input:'refused',error:`Text insert is too large (${route.length.toLocaleString()} characters, limit ${MAX_TEXT_INSERT.toLocaleString()}); nothing was inserted.`});}
      else if(route.kind==='commit'){runtime.ime(id,'commit',route.text);editor.value='';report({lastInput:'beforeinput'});}
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
    // LIVE_V1: remote cursors/drag ghosts on a DOM layer, remote carets through
    // the runtime's presence. Optional: absent live client or runtime export
    // leaves the desktop exactly as before.
    if(typeof options.live==='function'&&typeof runtime.live_view_snapshot==='function'){
      liveOverlay=createLiveOverlay({surface,canvas,live:options.live,
        snapshot:()=>active()?runtime.live_view_snapshot(id):null,
        applyRemote:rows=>active()?runtime.live_view_apply(id,JSON.stringify(rows)):false,
        setPublicationWindow:ms=>{if(active()&&typeof runtime.live_view_publication_window==='function')runtime.live_view_publication_window(id,ms??undefined);},
        members:()=>{const raw=options.liveMembers?.();return typeof raw==='string'?JSON.parse(raw):[];},
        presented:()=>lastState?.frames,
        onError:error=>options.onLiveError?.(error)});
      // Legacy windows the session streams, drawn over their tabs; needs WebCodecs.
      if(typeof globalThis.VideoDecoder==='function')legacyWindows=createLegacyWindows({surface,live:options.live,
        snapshot:()=>active()?runtime.live_view_snapshot(id):null,
        presented:()=>lastState?.frames,
        onError:error=>options.onLiveError?.(error)});
    }
    if(options.actionsV2||options.actionsV1)actionPalette=bindActionPalette({actions:options.actionsV2??options.actionsV1,publish:rows=>{if(active())runtime.caller_actions(id,JSON.stringify(rows));},report});
    if(options.actionsV2||options.actionsV1){desktopSpeculation=createDesktopSpeculation({runtime,id,active});desktopCommands=bindDesktopCommands({actions:options.actionsV2??options.actionsV1,onPending:pending=>{desktopSpeculation.pending(pending);navigation?.commandPending(pending);liveOverlay?.commandPending(pending);},onUpdate:row=>{navigation?.commandUpdated(row);desktopSpeculation.update(row);},peek:()=>{if(!active())return null;const raw=runtime.pending_desktop_command(id);return raw?JSON.parse(raw):null;},ack:invocation=>{if(active())runtime.acknowledge_desktop_command(id,invocation);},report:data=>{desktopSpeculation.report(data);report(data);}});}
    if(runtime.desktop_navigation_snapshot && runtime.desktop_navigation_snapshot(id)) {
      navigation=createDesktopNavigation({active,captureLaunchFocus:()=>({activation:inputScope.capture()?.activation,epoch:externalFocusEpoch}),snapshot:()=>runtime.desktop_navigation_snapshot(id),revision:()=>runtime.desktop_navigation_revision(id),dispatch:command=>{
        const request=navigationFocus.begin(JSON.parse(command),navigation.controller.getSnapshot());
        try{const result=runtime.desktop_navigate(id,command);navigationFocus.accepted(request);return result;}
        catch(error){navigationFocus.failed(request);throw error;}
      },search:(generation,query)=>runtime.desktop_command_search(id,generation,query),globalSearch:runtime.desktop_global_search?(generation,query)=>runtime.desktop_global_search(id,generation,query):undefined,focusSearch:()=>{inputScope.capture();canvas.focus({preventScroll:true});},captureSelect:()=>({scope:inputScope.capture(),epoch:externalFocusEpoch}),completeSelect:before=>handoff.complete(before.scope,before.epoch),
        // The local owner applies a command before desktop_navigate returns.
        awaitTerminal:local?async()=>{}:desktopCommands?invocation=>desktopCommands.waitForTerminal(invocation):undefined,
        install:async app=>{const appId=canonicalAppId(app);await apps.ensure(appId,install);return appId;}});
      unsubscribeDesktopCommits=options.subscribeDesktopCommits?.(event=>{if(active()){navigation.commandCommitted(event);desktopSpeculation?.committed(event);}});

      // Cloud's toolbar is a sibling of the renderer in its Desktop section.
      // Scope the listener to that mounted desktop; other page editors stay native.
      const shortcutScope=target.closest('section[aria-label="Desktop"]')??target;
      on(shortcutScope,'keydown',event=>{
        if(!active()||(!surface.contains(event.target)&&event.target.closest?.('input,textarea,[contenteditable="true"],[role="textbox"]')))return;
        routeGlobalSearchShortcut(event,composing,()=>navigation.openGlobalSearch());
      },{capture:true});
      options.onNavigation?.(navigation.controller);
    }
    const actionSurface=()=>{if(!active())throw Error('mount disposed');return id;};
    return {navigation: navigation?.controller,application,retryAsset,
      get record(){return lastState?.record||undefined;},
      get opened(){return opened;},
      actions:(resource,{window}={})=>JSON.parse(runtime.component_actions(actionSurface(),resource,window)),
      // An action may move the caret or edit: the live presence follows it.
      invokeAction:(resource,action,args={},{confirm=false,window}={})=>{const result=JSON.parse(runtime.invoke_component_action(actionSurface(),resource,action,JSON.stringify(args),confirm,window));liveOverlay?.refresh();return result;},
      openApp:(app,{record}={})=>{if(!navigation)throw Error('the mount is not a desktop');return navigation.controller.dispatch({kind:'open',generation:navigation.controller.getSnapshot().generation,app,...(record===undefined?{}:{record})});},
      cleanup:async()=>{registry.remove(token);await cleanup();},flush:()=>{persistenceFailed=false;return persist();},
      state:()=>lastState,canvas,get editor(){return editor;},semantics:accessibility,requestSemantics,

      durableState:()=>acquired.hydrate(),probePixels:async()=>JSON.parse(await runtime.probe_pixels(id)),failNextWrite:()=>{storage.abortNext=true;},destroyDevice:()=>runtime.destroy_device(id),recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args),
      liveSnapshot:()=>typeof runtime.live_view_snapshot==='function'&&active()?JSON.parse(runtime.live_view_snapshot(id)):null,
      liveOverlay:()=>liveOverlay?.stats()??null,
      legacyWindows:()=>legacyWindows?{...legacyWindows.stats(),streams:legacyWindows.streams()}:null};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
/** Every application this build can mount standalone. */
export async function applications(){await ready();return JSON.parse(runtime.applications());}
/** The loaded core's wasm-bindgen glue (loads the core if needed): what page modules such as relay.mjs call. */
export async function coreRuntime(){await ready();return runtime;}
export function pendingComponentCloseCount(){return runtime?.component_close_count()??0;}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));if(runtime)await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
