import {createMediaPump} from './voice-controls.mjs';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
import {createDataLoader} from './data-loader.mjs';
import {createFontDemandPump,createFontLoader} from './font-loader.mjs';
import init,* as runtime from './pkg/tilo_browser_runtime.js';
import {MountRegistry,applyEditorHints,createInputScope,createFocusHandoff,createCompositionSession,beforeInputRoute,MAX_TEXT_INSERT,oversizedInsert,createKeyboardForwarder,isDesktopSearchShortcut,keyboardRoute,modifierSnapshot,overlayKeyboardRoute,retainStatus,retainedWidget,pageWheelAsPixels,wheelNotchesAsLines} from './host-core.mjs';

import {acquireStorage} from './storage.mjs';
import {createDurableHost} from './durable-host.mjs';
import {semanticBridge} from './semantics.mjs';
import {recoverableClose} from './close-recovery.mjs';
import {createCrashQuarantine,crashAction} from './app-crash.mjs';
import {createDesktopNavigation} from './desktop-navigation.mjs';
import {createLiveCursors} from './live-cursors.mjs';
import {createHere} from './here.mjs';
import {createPresentedSource} from './panorama.mjs';
import {createSemanticPresentation} from './semantic-presentation.mjs';
export {createPanorama} from './panorama.mjs';
import {createLegacyWindows} from './legacy-windows.mjs';
import {createNodeApps} from './node-apps.mjs';
import {createNativeCatalogueBridge, invokeRoomMembership, runRoomAction} from './native-catalogue.mjs';
import {isWebKit,webgpuRefusal} from './webgpu-safety.mjs';
import {browserSettings} from './browser-locale.mjs';
// Page host for the Tilo browser runtime. Ported from the Tilo browser runtime
// (PR 541) without its desktop host and action palette.
// `mountDesktop` mounts the session desktop (tilo-desktop-surface over the
// desktop model) unless `app` names one application. A certified peer's
// `mountDesktop` passes itself as `peer`: edits are then attributed to its
// user, and its room's live layer shows members' pointers over the surface.
/** Application ids accepted by `mountDesktop({app})`; omit `app` for the desktop. */
export const APPLICATIONS=Object.freeze({desktop:'desktop',scratch:'so.tilo.browser-scratch'});
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
// The runtime's panic hook names the window whose application panicked; the
// module is dead after it, so each mount restarts the page without that window.
const crashes=createCrashQuarantine();
const crashListeners=new Set();
globalThis.__tiloRuntimePanic=raw=>{
  let crash;try{crash=JSON.parse(raw);}catch{return;}
  for(const listener of crashListeners)listener(crash);
};
const fontLoader=createFontLoader({baseURL:import.meta.url});
const fonts=retryableMemo(()=>fontLoader.startup());
const ready=retryableMemo(()=>init());
// Streaming compile starts on import, while the page connects; a failure retries on mount.
void ready().catch(()=>{});
const durableHost=createDurableHost({runtime,acquireStorage});
// Application data kept out of the module (the Typst editor's fonts), fetched when an app asks.
const data=createDataLoader({baseURL:import.meta.url,runtime});
/** Open a profile's durable storage for state kept outside any mount (the
 * relay peer). Shares the lease with mounts of the same profile/device;
 * `flush()` resolves when writes are durable; call `release()` once. */
/** The wasm runtime module, once loaded (the peer's codec and certificates). */
export async function loadRuntime(){await ready();return runtime;}
/** Open the WebGPU device and compile the renderer once the module is in,
 * while the page opens the workspace it was let into; the first mount
 * takes it.
 * Skipped where mounting would refuse WebGPU, and on WebKit, whose renderer
 * starts only once the page's load settled (`settleBeforeGpu`). */
export function prepareGpu(){
  if(!navigator.gpu||webgpuRefusal()||isWebKit(navigator))return;
  void ready().then(()=>runtime.prepare_gpu()).catch(()=>{});
}
/** Decode an image's bytes with the browser's own decoders into unpremultiplied
 * RGBA, scaled down to fit `maxDimension` on either side: the page's module
 * carries no image decoders of its own. */
async function decodeImage(bytes,maxDimension){
  const source=await createImageBitmap(new Blob([bytes]),{premultiplyAlpha:'none'});
  try{
    const scale=Math.min(1,maxDimension/Math.max(source.width,source.height));
    const width=Math.max(1,Math.round(source.width*scale)),height=Math.max(1,Math.round(source.height*scale));
    const canvas=new OffscreenCanvas(width,height);
    const context=canvas.getContext('2d',{willReadFrequently:true});
    context.imageSmoothingQuality='high';
    context.drawImage(source,0,0,width,height);
    return {width,height,rgba:new Uint8Array(context.getImageData(0,0,width,height).data.buffer)};
  }finally{source.close();}
}
/** Fetch and compile the module of application `app` beside the runtime's
 * own, so the app the page is about to show does not wait for the start
 * module before its code even starts to download. A bundle that splits its
 * apps (the desktop bundle) names each app's module in `apps.json`; any
 * other bundle, an unknown app or a failed fetch leaves it to the app's
 * first open, as without this. */
export function prefetchApplication(app,{fetch=globalThis.fetch,load=()=>import('./pkg/__wasm_split.js')}={}){
  void (async()=>{
    const listing=await fetch(new URL('apps.json',import.meta.url));
    if(!listing.ok)return;
    const module=(await listing.json()).applications?.find(entry=>entry.id===app)?.module;
    if(typeof module==='string')(await load()).prefetch(module);
  })().catch(()=>{});
}
export async function openProfileStorage(profile,device,options={}){await ready();return durableHost.open(profile,device,options);}
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='tilo-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;overscroll-behavior:none;';
  // Native focus may scroll hidden semantic controls; it must not move the canvas viewport.
  const surface=document.createElement('div');surface.className='tilo-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:clip;overscroll-behavior:none';
  let editor=document.createElement('textarea');editor.setAttribute('aria-label','Tilo focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none';
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','Tilo application');accessibility.style.cssText='position:absolute;top:0;left:0;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);';
  const commandStatus=document.createElement('div');commandStatus.setAttribute('role','status');commandStatus.setAttribute('aria-live','polite');
  commandStatus.style.cssText='display:none;position:absolute;bottom:12px;left:12px;max-width:80%;padding:8px 12px;background:#20242e;color:#fff;border-radius:8px;font:13px system-ui;pointer-events:none';
  surface.append(canvas,editor,accessibility,commandStatus);target.append(surface);
  let id,storage,closed=false,crashed=false,composing=false,pasteShift=false,flush=Promise.resolve(),lastState,readyResolve,readyReject,persistenceFailed=false;
  const presented=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  void presented.catch(()=>{});
  const report=data=>{
    if(data.clipboard==='copied'||data.clipboard==='failed'){commandStatus.style.display='block';commandStatus.textContent=data.clipboard==='copied'?'Copied to system clipboard':`Could not copy to system clipboard: ${data.clipboardError??'browser access denied'}`;}
    if(data.desktopCommand){const row=data.desktopCommand;commandStatus.style.display='block';commandStatus.textContent=row.state==='succeeded'?'Desktop change saved':row.state==='submitting'||row.state==='accepted'?'Saving desktop change…':row.state==='failed'||row.state==='rejected'?`Desktop change refused: ${row.error??'unavailable'}`:`Desktop change awaiting confirmation: ${row.error??row.state}`;}
    if(data.desktopErrors?.length){commandStatus.style.display='block';commandStatus.textContent=`Desktop request unavailable: ${data.desktopErrors.at(-1)}`;}
    options.onStatus?.(data);
  };
  const navigationFocus={errors(){},update(){},close(){}};
  const presentationWaiters=new Set();
  const config={profile:'tilo-browser',device:'tilo-browser-device',room:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',author:options.peer?undefined:'Browser user',instance:1,mac:/Mac/.test(navigator.platform),conservativeGpu:isWebKit(navigator),...options};
  // Model credentials are mount-scoped bootstrap input, never a page global.
  // The WASM host binds the authenticated transport before each app factory.
  if(options.models)config.models={endpoint:options.models.endpoint,sessionToken:options.models.sessionToken,profileId:options.models.profileId,modelId:options.models.modelId};
  // Administration is opt-in and never borrows the inference session token.
  if(options.providerManagement)config.providerManagement={endpoint:options.providerManagement.endpoint,sessionToken:options.providerManagement.sessionToken};
  delete config.peer;if(options.peer?.runtimeId!==undefined)config.peer=options.peer.runtimeId;delete config.settings;delete config.onNavigation;delete config.onStatus;delete config.signal;delete config.loadAsset;delete config.actionsV1;delete config.subscribeDesktopCommits;delete config.live;delete config.liveMembers;delete config.restartOnCrash;delete config.presenceUrl;
  // The desktop offers Invite only when the page takes it.
  config.invite=typeof options.onInvite==='function';delete config.onInvite;
  // What only this page can do for the person (keeping their identity): offered in desktop search and on banners.
  delete config.hostActions;delete config.onHostAction;
  let unsubscribeDesktopCommits;
  let liveOverlay,liveCursors,liveCarets,here,legacyWindows,nodeApps,livePlace=null;
  // The room's live layer as this mount publishes through it (a certified
  // peer's `live` view: only the mount acted in last speaks), else the peer's.
  const liveLayer=()=>typeof options.live==='function'?options.live():options.peer?.live;
  const semanticPresentation=createSemanticPresentation({request:(target,observe)=>{if(active())runtime.request_component_locus(id,JSON.stringify(target),observe);},valid:()=>fontMountValid(),surfaceSize:()=>[canvas.width,canvas.height]});
  const capture=createPresentedSource({audience:JSON.stringify([config.profile,config.device,config.room]),app:config.app,canvas,layer:liveLayer,valid:()=>fontMountValid()});
  let media,semantic,semanticKeys,hiddenKeys,composition,actionPalette,desktopCommands,desktopSpeculation,navigation,fontDemands,externalFocusEpoch=0,runtimeReady=false,desktopOwnership=false,localActivationGesture;
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
  // Focus passing from the canvas to its own IME editor stays in the surface.
  // Winit reads every canvas blur as the window's: stopped before its listener
  // (registered at mount), so a press's IME handoff keeps the pointer gesture
  // the press began, such as a block drag.
  on(canvas,'blur',event=>{if(event.relatedTarget===editor)event.stopImmediatePropagation();},{capture:true});
  function newComposition(){return createCompositionSession({scope:inputScope,send:(kind,text)=>runtime.ime(id,kind,text),clear:()=>{editor.value='';composing=false;}});}
  function replaceEditor(restoreFocus=true){
    const old=editor,focused=restoreFocus&&document.activeElement===old;
    editor=old.cloneNode(false);editor.value='';
    for(const [type,fn,opts] of editorListeners)on(editor,type,fn,opts);
    composition=newComposition();old.replaceWith(editor);
    if(focused){const scope=inputScope.capture();if(scope)(scope.legacy||scope.ime?editor:canvas).focus({preventScroll:true});}
  }
  function active(){return !closed&&!crashed&&!surface.inert&&registry.current(token)&&id!==undefined&&runtimeReady;}
  const nativeCatalogue = createNativeCatalogueBridge({current:active,
    actions:()=>options.peer?.actions,
    membership:()=>options.peer?.membership,
    take:()=>JSON.parse(runtime.component_catalogue_requests?.(id) ?? '[]'),
    deliver:message=>runtime.component_catalogue_response(id,JSON.stringify(message)),
    onError:error=>report({catalogueError:String(error)})});
  const fontMountValid=()=>!closed&&!crashed&&registry.current(token)&&id!==undefined&&runtimeReady;
  fontDemands=createFontDemandPump({loader:fontLoader,valid:fontMountValid,report,install:(name,bytes)=>runtime.install_font(id,name,bytes),failed:()=>runtime.font_failed(id)});
  function update(raw){
    if(closed||crashed||!registry.current(token))return;
    const state=JSON.parse(raw);if(state.clipboard){report(state);return;}
    nativeCatalogue.pump();
    for(const request of state.hostRequests??[]){if(request==='invite')options.onInvite?.();else if(request?.action)options.onHostAction?.(request.action);else if(typeof request?.named==='string')options.onHostAction?.('set-name',request.named);else if(request?.['room-action']){const asked=request['room-action'];void runRoomAction(options.peer?.actions,asked).then(answer=>{if(active())runtime.desktop_room_action_answer(id,asked.id,JSON.stringify(answer));});}}
    if('focusPresence' in state)liveLayer()?.publishPresence({selection:state.focusPresence});
    if('livePlace' in state){livePlace=state.livePlace;liveLayer()?.publishPresence({focus:livePlace?.focus??null,place:livePlace?.place??null});}
    if(!runtimeReady&&['gpu-pending','presented'].includes(state.phase)){
      desktopOwnership=Boolean(runtime.desktop_input_ownership&&runtime.desktop_navigation_snapshot?.(id));
      runtimeReady=true;
      if(options.hostActions&&runtime.desktop_set_host_actions&&runtime.desktop_navigation_snapshot?.(id))runtime.desktop_set_host_actions(id,JSON.stringify(options.hostActions));
      if(runtime.desktop_navigation_snapshot(id)){
        navigation=createDesktopNavigation({snapshot:()=>runtime.desktop_navigation_snapshot(id),revision:()=>runtime.desktop_navigation_revision(id),dispatch:command=>runtime.desktop_navigate(id,command),active});
        options.onNavigation?.(navigation.controller);
      }
    }
    inputScope.capture();
    if(runtimeReady)data.pump(report);
    if(runtimeReady&&options.peer?.voice){media??=createMediaPump({runtime,id,voice:options.peer.voice,room:config.room,active,report});queueMicrotask(()=>{if(active())media.pump();});}
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
    // Search opened from page chrome (a Search button) takes the keyboard.
    if(state.searchOpen&&!lastState?.searchOpen&&![canvas,editor].includes(document.activeElement))canvas.focus({preventScroll:true});
    lastState=retainStatus(lastState,state);report(state);
    if(state.phase==='suspended'){surface.inert=true;editor.disabled=true;inputScope.capture();}

    if(state.phase==='presented'){surface.inert=false;editor.disabled=false;liveOverlay?.wake();const layer=liveLayer();if(!liveCursors&&layer){liveCursors=createLiveCursors({layer,surface,localFocus:()=>livePlace?.focus??null,windows:()=>{const shown=runtime.desktop_record_windows?.(id);return shown?JSON.parse(shown):undefined;}});liveCarets=followRemoteCarets(layer,()=>id,active);if(livePlace)layer.publishPresence({focus:livePlace.focus,place:livePlace.place});}if(!here&&!config.app&&options.peer&&runtime.desktop_here_tick)here=createHere({runtime,mountId:()=>id,active,layer:liveLayer,peer:options.peer,presenceUrl:options.presenceUrl,onError:error=>report({hereError:String(error)})});if(!legacyWindows&&layer&&typeof options.peer?.legacyInput==='function'&&runtime.desktop_set_peer_windows&&runtime.desktop_peer_windows(id)!==undefined)legacyWindows=createLegacyWindows({surface,live:liveLayer,windows:()=>{const shown=active()&&runtime.desktop_peer_windows(id);return shown?JSON.parse(shown):[];},setWindows:list=>{if(active())runtime.desktop_set_peer_windows(id,JSON.stringify(list));},sendInput:(publisher,stream,events)=>options.peer.legacyInput(publisher,stream,events),dismissed:()=>active()&&runtime.desktop_dismissed_peer_windows?JSON.parse(runtime.desktop_dismissed_peer_windows(id)):[],paintFrame:(key,bytes)=>Boolean(active()&&runtime.desktop_peer_window_paint?.(id,key,bytes)),takePaintInput:()=>active()&&runtime.desktop_take_peer_window_input?JSON.parse(runtime.desktop_take_peer_window_input(id)):[],focusWindow:key=>{if(active()&&runtime.desktop_focus_peer_window)runtime.desktop_focus_peer_window(id,key);},focusDesktop:()=>{if(active())canvas.focus({preventScroll:true});},onError:error=>report({legacyWindowError:String(error)})});legacyWindows?.wake();if(!nodeApps&&typeof options.peer?.nodeSession==='function'&&options.peer.agents&&runtime.desktop_set_node_apps)nodeApps=createNodeApps({room:config.room,nodeSession:()=>options.peer.nodeSession(),actions:device=>options.peer.agents.actions(device),setApps:apps=>{if(active())runtime.desktop_set_node_apps(id,JSON.stringify(apps));},takeRequests:()=>active()?JSON.parse(runtime.desktop_take_node_app_requests(id)):[],onError:error=>report({nodeAppsError:String(error)})});}
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
      applyEditorHints(editor,ime.hints);
      if(document.activeElement===canvas)editor.focus({preventScroll:true});
    }else if(document.activeElement===editor){canvas.focus({preventScroll:true});}
    observeLocalActivation();
    if(state.phase==='presented'){readyResolve();void persist().catch(()=>{});void loadAssets();}
  }
  const assets=createAssetRecovery({active,report,
    retry:()=>false,
    complete:(request,image)=>runtime.complete_asset(id,request.loader,request.url,image?.width??0,image?.height??0,image?.rgba),
    load:async request=>decodeImage(await fetchAsset(request),request.maxDimension),
  });
  async function fetchAsset(request){
    if(options.loadAsset)return options.loadAsset(request.url,listeners.signal);
    // A content address: the runtime hands one out only once its bytes are here.
    if(request.url.startsWith('tilo-blob://')){
      const bytes=runtime.blob_asset(id,request.loader,request.url);
      if(!bytes)throw Error('asset unavailable: content not here');
      return bytes;
    }
    const url=new URL(request.url,location.href);
    if(!['http:','https:'].includes(url.protocol))throw Error('asset unavailable: no admitted content transport');
    // A cross-origin image goes through the host's fetch proxy when it offers
    // one (tilo-net's wire protocol v1), else directly where the site allows
    // it; the page's cookies never go to another origin.
    const proxy=url.origin===location.origin?undefined:globalThis.__tiloFetchProxy;
    const response=typeof proxy==='string'
      ?await fetch(`${new URL(proxy,location.href).href}?url=${encodeURIComponent(url.href)}`,{signal:listeners.signal,credentials:'same-origin',headers:{'x-tilo-proxy':'1'}})
      :await fetch(url,{signal:listeners.signal,credentials:url.origin===location.origin?'same-origin':'omit'});
    if(!response.ok)throw Error(`asset HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  }
  async function loadAssets(){
    if(!active())return;
    await Promise.allSettled(JSON.parse(runtime.asset_requests(id)).map(request=>assets.request(request)));
  }
  // `font:NAME` and `data:NAME` name a font and an application data file;
  // any other id an image request of this mount.
  async function retryAsset(assetId){
    if(!active())throw Error('Asset surface disposed');
    const id=String(assetId);
    if(id.startsWith('font:')){await fontDemands.retry(id.slice(5));return;}
    if(id.startsWith('data:')){await data.retry(id.slice(5),report);return;}
    await assets.recover(id);
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
      // pending_writes also moves the record's unsaved edits into the store.
      const writes=Array.from(runtime.pending_writes(current));
      try{
        await runtime.flush_files();await storage.commit(writes);await storage.flush();
        report({persistence:'committed',pendingWrites:0,error:null});
      }catch(error){
        persistenceFailed=true;report({persistence:'failed',pendingWrites:storage.status.pendingWrites,error:String(error)});throw error;
      }
    });
    return flush;
  }
  function onCrash(crash){
    // The module is stopped: nothing on this page may call into it again.
    crashed=true;surface.inert=true;capture.close();semanticPresentation.close();
    const action=crashAction(crash,crash.tab!=null&&crashes.record(config.room,crash));
    report({phase:'crashed',crashedWindow:crash.tab??null,error:crash.message,restarting:action==='restart'&&options.restartOnCrash!==false});
    if(action==='restart'&&options.restartOnCrash!==false)setTimeout(()=>location.reload(),250);
  }
  let closeWrites=[];
  const cleanup=recoverableClose({
    begin(){
      capture.close();semanticPresentation.close();
      crashListeners.delete(onCrash);
      nativeCatalogue.dispose();media?.close();liveOverlay?.close();liveOverlay=undefined;liveCursors?.close();liveCursors=undefined;liveCarets?.();liveCarets=undefined;here?.stop();here=undefined;legacyWindows?.close();legacyWindows=undefined;nodeApps?.close();nodeApps=undefined;unsubscribeDesktopCommits?.();navigationFocus.close();navigation?.close();actionPalette?.close();desktopCommands?.close();desktopSpeculation?.close();semantic.close();closed=true;for(const waiter of presentationWaiters)waiter.reject(Error('mount disposed'));presentationWaiters.clear();listeners.abort();readyReject(Error('mount disposed'));
      if(id!==undefined)closeWrites=Array.from(runtime.close(id));
      surface.remove();
    },
    async settle(){
      await flush.catch(()=>{});
      await runtime.flush_files();
      if(storage&&closeWrites.length){await storage.commit(closeWrites);closeWrites=[];}
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
    const refusal=webgpuRefusal();if(refusal)throw Error(refusal);
    if(!navigator.gpu)throw Error('WebGPU is required for the Tilo Vello compute renderer');
    const startupFonts=fonts();void startupFonts.catch(()=>{});
    await ready();if(!registry.current(token))throw Error('mount disposed');
    const acquired=await durableHost.open(config.profile,config.device,{settings:options.settings??browserSettings()});
    if(closed||!registry.current(token)){await acquired.release();throw Error("mount disposed");}
    storage=acquired;
    // Apps' own files (OPFS) are mirrored before any app reads them.
    const [assets,entries]=await Promise.all([startupFonts,storage.hydrate(),runtime.mirror_files(config.profile)]);
    if(!registry.current(token))throw Error('mount disposed');
    // WebKit's GPU process holds the page's load-time surfaces for a moment;
    // starting the renderer (and its pipeline compilation) on top of them
    // stacks two peaks under Safari's 600 MB limit.
    if(config.conservativeGpu)await settleBeforeGpu();
    if(!registry.current(token))throw Error('mount disposed');
    // An app mounted alone has its code fetched first when the bundle loads
    // it on demand; the session desktop fetches each app as its window opens.
    if(config.app&&!await runtime.load_application(config.app))throw Error(`${config.app} could not be loaded`);
    if(!registry.current(token))throw Error('mount disposed');
    runtime.quarantine_windows?.(JSON.stringify(crashes.entries(config.room)));
    crashListeners.add(onCrash);
    id=runtime.mount(canvas,JSON.stringify(config),assets,entries,raw=>{
      // Pin pixels and geometry while this presentation is still current,
      // before another presentation or a queued page callback can replace it.
      // This path reads only JS/Canvas state, never re-enters the WASM mount.
      const state=JSON.parse(raw);
      if(state.phase){if(state.presentation===true)state.observedPresentedAtMs=performance.now();semanticPresentation.present(state);capture.present(state);}
      queueMicrotask(()=>update(raw));
    });
    inputScope.capture();
    const publishModifiers=event=>{if(inputScope.capture())runtime.modifiers(id,...modifierSnapshot(event));};
    const clearModifiers=()=>{if(inputScope.capture())runtime.modifiers(id,false,false,false,false);};
    for(const type of ['pointerdown','focusin','keydown'])on(document,type,event=>{if(!surface.contains(event.target))externalFocusEpoch++;},{capture:true});
    // Desktop search's chord is the desktop's wherever focus is on the page:
    // the browser must not take it (Ctrl+K focuses its address bar).
    on(document,'keydown',event=>{
      if(!active()||!navigation||!isDesktopSearchShortcut(event,config.mac))return;
      event.preventDefault();event.stopImmediatePropagation();
      runtime.desktop_search_toggle(id);canvas.focus({preventScroll:true});report({lastInput:'search'});
    },{capture:true});
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
    const dispatchCanvasKey=(key,mods={})=>{canvas.dispatchEvent(new KeyboardEvent('keydown',{key,...mods}));canvas.dispatchEvent(new KeyboardEvent('keyup',{key,...mods}));};
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
      else if(route.kind==='key'){dispatchCanvasKey(route.key,{ctrlKey:Boolean(route.ctrlKey)});report({lastInput:'beforeinput-key'});}
    });
    onEditor('input',()=>{if(!composition.composing)editor.value='';});
    let editorFocusScope,canvasFocusScope;
    onEditor('focus',()=>{editorFocusScope=inputScope.capture();if(!editorFocusScope)return;canvas.dispatchEvent(new FocusEvent('focus'));runtime.ime(id,'start','');});
    onEditor('blur',event=>{
      if(!inputScope.valid(editorFocusScope))return;
      if(composition.composing){runtime.ime(id,'preedit','');composition.cancel();composing=false;replaceEditor(false);report({lastInput:'compositioncancel'});}
      hiddenKeys.blur(hiddenKeyOwner);runtime.ime(id,'end','');if(event.relatedTarget!==canvas)canvas.dispatchEvent(new FocusEvent('blur'));
    });
    on(canvas,'focus',()=>{canvasFocusScope=inputScope.capture();});
    on(canvas,'blur',()=>{if(inputScope.valid(canvasFocusScope))clearModifiers();});
    // Throttled statuses omit the tree; assistive technology entering the surface gets the complete current tree at once.
    const requestSemantics=()=>{if(active())runtime.request_semantics?.(id);};
    on(accessibility,'focusin',event=>{if(!accessibility.contains(event.relatedTarget))requestSemantics();});

    on(canvas,'pointerdown',()=>report({lastInput:'pointer'}));
    // Every wheel over the surface is the runtime's: sideways swipes pan the
    // strip or scroll an app, Ctrl+wheel is pinch zoom. None may scroll,
    // zoom or navigate the page's history.
    // A mouse wheel's notches reach the runtime as the lines a native wheel
    // sends; this runs before the runtime's own canvas listener.
    const wheelNotch={x:0,y:0};
    on(surface,'wheel',event=>{
      if(event.target!==canvas)return;
      const lines=wheelNotchesAsLines(event,config.mac,wheelNotch);
      if(!lines)return;
      event.preventDefault();event.stopImmediatePropagation();
      canvas.dispatchEvent(new WheelEvent('wheel',lines));
    },{capture:true,passive:false});
    on(canvas,'wheel',event=>{
      event.preventDefault();
      const pixels=pageWheelAsPixels(event,canvas.clientWidth,canvas.clientHeight);
      if(pixels)canvas.dispatchEvent(new WheelEvent('wheel',pixels));
      else report({lastInput:'wheel'});
    },{passive:false});
    on(document,'visibilitychange',()=>{if(active())runtime.visible(id,document.visibilityState==='visible');});
    await presented;
    if(!active())throw Error('mount disposed');
    return {retryAsset,cleanup:async()=>{registry.remove(token);await cleanup();},flush:()=>{persistenceFailed=false;return persist();},
      state:()=>lastState,canvas,captureSource:capture.handle,observeSemanticTarget:target=>semanticPresentation.observe(target),get editor(){return editor;},semantics:accessibility,requestSemantics,
      get navigation(){return navigation?.controller;},
      // Host actions (`[{id, title, subtitle?}]`) in place of earlier ones, and a banner offering one (`{action, title, body}`).
      setHostActions:actions=>{if(active()&&runtime.desktop_set_host_actions)runtime.desktop_set_host_actions(id,JSON.stringify(actions));},
      offerHostAction:offer=>{if(active()&&runtime.desktop_offer_host_action)runtime.desktop_offer_host_action(id,JSON.stringify(offer));},
      // What the page just did out of sight, said in passing ("Link copied").
      acknowledge:text=>{if(active()&&runtime.desktop_acknowledge)runtime.desktop_acknowledge(id,String(text));},
      // Who the desktop shows as here (`[{id, label, agent}]`, one row per person), or null for a single app.
      people:()=>{const shown=active()&&runtime.desktop_here_people?.(id);return shown?JSON.parse(shown):null;},

      durableState:()=>storage?.hydrate(),failNextWrite:()=>{storage.lease.abortNext=true;},recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args),
      publications:()=>JSON.parse(runtime.component_publications(id)),
      actions:(resource,{window}={})=>JSON.parse(runtime.component_actions(id,resource??undefined,window)),
      // The room's own membership actions are the host's, not an app's: a promise of the host's answer.
      invokeAction:(resource,action,args={},{confirm=false,window}={})=>invokeRoomMembership({membership:options.peer?.membership,room:options.peer?.room,resource,action,args,confirm})
        ??JSON.parse(runtime.invoke_component_action(id,resource,action,JSON.stringify(args),confirm,window)),};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
async function settleBeforeGpu(){
  if(document.readyState!=='complete')await new Promise(resolve=>addEventListener('load',resolve,{once:true}));
  await new Promise(resolve=>setTimeout(resolve,1500));
  await new Promise(resolve=>(globalThis.requestIdleCallback??(f=>setTimeout(f,0)))(resolve,{timeout:1500}));
}
// Members' carets and selections, with their names, into the mount's
// editors. Only a changed selection or participant crosses into the runtime.
function followRemoteCarets(layer,mountId,active){
  const last=new Map();
  const apply=({publisher,frame})=>{
    if(!active())return;
    const participant=frame.participant??last.get(publisher)?.participant??null;
    const key=JSON.stringify([frame.selection,participant]);
    if(last.get(publisher)?.key===key)return;
    last.set(publisher,{key,participant});
    runtime.remote_presence(mountId(),publisher,participant?.label??'',participant?.kind??'person',participant?.color??'',frame.selection?.anchor,frame.selection?.head);
  };
  const stops=[layer.onPresence(apply),layer.onMember(({publisher,active:present})=>{
    if(present||!last.delete(publisher)||!active())return;
    runtime.remote_presence_gone(mountId(),publisher);
  })];
  for(const remote of layer.remoteState())apply({publisher:remote.publisher,frame:remote});
  return ()=>{for(const stop of stops)stop();};
}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
