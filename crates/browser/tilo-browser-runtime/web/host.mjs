import {createMediaPump} from './voice-controls.mjs';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
import {createDataLoader} from './data-loader.mjs';
import {createImageDecoder} from './image-decode.mjs';
import {createFontDemandPump,createFontLoader} from './font-loader.mjs';
import init,* as runtime from './pkg/tilo_browser_runtime.js';
import {MountRegistry,applyEditorHints,createScreenFill,createInputScope,createFocusHandoff,createCompositionSession,beforeInputRoute,MAX_TEXT_INSERT,oversizedInsert,createKeyboardForwarder,draggingFiles,droppedFiles,pastedPicture,isDesktopSearchShortcut,keyboardRoute,modifierSnapshot,overlayKeyboardRoute,retainStatus,retainedWidget,createHostStatus,pageWheelAsPixels,wheelNotchesAsLines,wheelPointerMove} from './host-core.mjs';

import {acquireStorage} from './storage.mjs';
import {createDurableHost} from './durable-host.mjs';
import {semanticBridge,applySemanticsPatch} from './semantics.mjs';
import {recoverableClose} from './close-recovery.mjs';
import {createCrashQuarantine,createPageRestarts,crashAction,pageCrashAction,showPageCrash,trapCrash} from './app-crash.mjs';
import {createDesktopNavigation} from './desktop-navigation.mjs';
import {createHere} from './here.mjs';
import {fetchPersonProfile,followPhotos,photoPixels} from './profile-photos.mjs';
import {createPresentedSource} from './panorama.mjs';
import {createSemanticPresentation} from './semantic-presentation.mjs';
import {createDomPresenter} from './dom-present.mjs';
export {createPanorama} from './panorama.mjs';
import {createNativeCatalogueBridge, invokeMembershipAction} from './native-catalogue.mjs';
import {isWebKit,webgpuRefusal} from './webgpu-safety.mjs';
import {browserSettings} from './browser-locale.mjs';
import {fetchProxyFor,proxiedGet} from './fetch-proxy.mjs';
// Page host for the Tilo browser runtime. Ported from the Tilo browser runtime
// (PR 541) without its desktop host and action palette.
// `mountDesktop` mounts the session desktop (tilo-desktop-surface over the
// desktop model) unless `app` names one application. A certified peer's
// `mountDesktop` passes itself as `peer`: edits are then attributed to its
// user, and its workspace's live layer shows members' pointers over the surface.
/** Application ids accepted by `mountDesktop({app})`; omit `app` for the desktop. */
export const APPLICATIONS=Object.freeze({desktop:'desktop'});
let registry=new MountRegistry();
const closures=new Set();
const recoveryJobs=new Set();
// The runtime's panic hook names the window whose application panicked; the
// module is dead after it, so each mount restarts the page without that window.
const crashes=createCrashQuarantine();
const crashListeners=new Set();
// A panic that names no window leaves no application to close: the page
// itself is what starts over (`app-crash.mjs`), once.
const pageRestarts=createPageRestarts();
let pageCrashed=false;
/** Where the page reports what it did not catch, for the door it came in
 * through to log (`entry-page.mjs`). A panic is reported the same way. */
let pageErrorReport=null;
export function reportPageErrors(report){pageErrorReport=report;}
let runtimePanicked=false;
globalThis.__tiloRuntimePanic=raw=>{
  runtimePanicked=true;
  let crash;try{crash=JSON.parse(raw);}catch{return;}
  for(const listener of crashListeners)listener(crash);
  if(crash?.tab!=null||pageCrashed)return;
  pageCrashed=true;
  // The report leaves first, as a crashed window's does; the load that follows
  // shows the workspace, its windows and their drafts as the session saved them.
  pageCrashAction(crash,{restarts:pageRestarts,reload:()=>setTimeout(()=>location.reload(),250),
    show:message=>showPageCrash(message),reportError:(kind,message)=>pageErrorReport?.(kind,message)});
};
// A trap no panic preceded (`trapCrash`) is a panic outside any window too.
globalThis.addEventListener?.('error',event=>{
  const crash=trapCrash(event.error,{panicked:runtimePanicked});
  if(crash)globalThis.__tiloRuntimePanic(JSON.stringify(crash));
});
// One startup timeline from navigation start, so a slow phase names itself
// instead of hiding in "loading". tools/perf/web_startup.mjs reads it.
const mark=name=>{try{performance.mark(`tilo:${name}`);}catch{}};
mark('module');
const fontLoader=createFontLoader({baseURL:import.meta.url});
const fonts=retryableMemo(()=>fontLoader.startup());
const ready=retryableMemo(()=>init());
// Streaming compile starts on import, while the page connects; a failure retries on mount.
void ready().catch(()=>{});
// So do the fonts a mount waits for: fetched beside the module, not after the page joined.
void fonts().catch(()=>{});
const durableHost=createDurableHost({runtime,acquireStorage});
// Application data kept out of the module (the Typst editor's fonts), fetched when an app asks.
const data=createDataLoader({baseURL:import.meta.url,runtime});
/** Open a profile's durable storage for state kept outside any mount (the
 * relay peer). Shares the lease with mounts of the same profile/device;
 * `flush()` resolves when writes are durable; call `release()` once. */
/** The certified peer's code — its crypto, transport and the workspaces'
 * replication — is a module of its own in the desktop bundle (`lazy_exports!`
 * in tilo-web-desktop), which nothing before a workspace's door needs. It is
 * fetched as soon as the start module is in, and every door waits for it.
 * The bindgen glue of a split bundle imports `__wasm_split.js` itself, so it
 * is here once the start module is; a bundle without one has the peer in
 * its start module. */
const peerCode=retryableMemo(async()=>{
  await ready();
  const split=await import('./pkg/__wasm_split.js').catch(()=>null);
  await split?.load('peer');
});
void peerCode().catch(()=>{});
/** The wasm runtime module, once loaded with the peer's code (its codec and
 * certificates). */
export async function loadRuntime(){await peerCode();return runtime;}
/** Open the WebGPU device and compile the renderer once the module is in,
 * while the page opens the workspace it was let into; the first mount
 * takes it.
 * Skipped where mounting would refuse WebGPU, and on WebKit, whose renderer
 * starts only once the page's load settled (`settleBeforeGpu`). */
export function prepareGpu(){
  if(!navigator.gpu||webgpuRefusal()||isWebKit(navigator))return;
  void ready().then(()=>runtime.prepare_gpu()).catch(()=>{});
}
/** Every mount's images decode in one worker, started on the first. */
let imageDecoder;
function decodeImage(bytes,maxDimension){
  imageDecoder??=createImageDecoder();
  return imageDecoder(bytes,maxDimension);
}
/** Fetch and compile the module of application `app` once the runtime's
 * own is in, before the page mounts it, so the app the page is about to
 * show does not wait for its first open. App code is not fetched beside the
 * runtime: on a link the downloads saturate, the runtime then arrives late,
 * and admission, which needs it, runs only after both instead of while the
 * app's code downloads. A bundle that splits its apps (the desktop bundle)
 * names each app's module in `apps.json`; any other bundle, an unknown app
 * or a failed fetch leaves it to the app's first open, as without this.
 *
 * `apps.json` is a few kilobytes and needs nothing from the runtime, so it
 * is fetched beside it and remembered: the app's own module then starts as
 * soon as the runtime is in, instead of after the runtime and a round trip
 * for the listing one after the other. */
let applicationModules;
function applications(fetch){
  return applicationModules??=fetch(new URL('apps.json',import.meta.url))
    .then(listing=>listing.ok?listing.json():null)
    .then(listed=>listed?.applications??[])
    .catch(()=>[]);
}
export function prefetchApplication(app,{fetch=globalThis.fetch,load=()=>import('./pkg/__wasm_split.js'),after=ready}={}){
  const listing=applications(fetch);
  // A failure is the app's first open to report, as if this were never here.
  void listing.catch(()=>{});
  void (async()=>{
    const [modules]=await Promise.all([listing,after()]);
    const module=modules.find(entry=>entry.id===app)?.module;
    if(typeof module==='string')(await load()).prefetch(module);
  })().catch(()=>{});
}
/** The name `apps.json` gives `app`, or `null`: what the bar over one app calls it. */
export async function applicationName(app,{fetch=globalThis.fetch}={}){
  const listed=(await applications(fetch)).find(entry=>entry.id===app);
  return typeof listed?.name==='string'?listed.name:null;
}
/** How many apps' modules `prefetchOfferedApplications` fetches and compiles
 * at once. A desktop offers dozens; fetching them all at once saturates the
 * link the person's own work shares, so a bounded few go at a time, each in
 * an idle period, and the rest wait for one to finish. */
const PREFETCH_AT_ONCE=2;
/** Fetch and compile the modules of every app the desktop offers, in the
 * background after the page has drawn its first frame.
 *
 * The desktop bundle names each app's module in `apps.json`, and an app
 * whose module is not yet in has nothing to draw when the person opens it:
 * its window appears at once but stays a placeholder until the module lands
 * — on a cold page, the seconds before an app the person chose shows
 * anything. The apps the desktop offers do not depend on what the person
 * opens, so their modules are fetched ahead of time, starting after the
 * first frame so nothing of it delays what the person sees. It never
 * competes: [`PREFETCH_AT_ONCE`] modules at a time, each in the page's idle
 * period (or a timeout when it is never idle), and the browser's HTTP cache
 * and the splitter's own compiled-module cache keep what came, so an app
 * opened later instantiates without fetching or compiling again.
 *
 * An app the listing does not name, or a module that fails, is left to the
 * app's first open, as without this: a prefetch never breaks a page. It runs
 * once; asking again is the same run. */
let offeredPrefetch;
export function prefetchOfferedApplications({fetch=globalThis.fetch,load=()=>import('./pkg/__wasm_split.js'),after=ready,idle=globalThis.requestIdleCallback??(run=>setTimeout(()=>run({timeRemaining:()=>4}),0))}={}){
  offeredPrefetch??=(async()=>{
    const [modules]=await Promise.all([applications(fetch),after()]);
    const names=modules.map(entry=>entry.module).filter(name=>typeof name==='string');
    if(!names.length)return;
    const compiled=await load();
    let next=0;
    const worker=async()=>{
      while(next<names.length){
        const name=names[next++];
        await new Promise(resolve=>idle(resolve));
        try{await compiled.prefetch(name);}catch{}
      }
    };
    await Promise.all(Array.from({length:Math.min(PREFETCH_AT_ONCE,names.length)},worker));
  })().catch(()=>{});
  return offeredPrefetch;
}
export async function openProfileStorage(profile,device,options={}){await ready();return durableHost.open(profile,device,options);}
export async function mountDesktop(target,options={}) {
  const token=registry.begin(target);
  const canvas=document.createElement('canvas');canvas.className='tilo-canvas';canvas.tabIndex=0;
  canvas.style.cssText='display:block;width:100%;height:100%;outline:none;touch-action:none;overscroll-behavior:none;';
  // Native focus may scroll hidden semantic controls; it must not move the canvas viewport.
  const surface=document.createElement('div');surface.className='tilo-host';surface.style.cssText='position:relative;width:100%;height:100%;overflow:clip;overscroll-behavior:none';
  let editor=document.createElement('textarea');editor.setAttribute('aria-label','Tilo focused text editor');editor.tabIndex=-1;editor.autocomplete='off';editor.autocapitalize='off';editor.spellcheck=false;
  // The canvas paints the caret and the text; the IME editor sits at the caret only so the
  // browser places its composition UI there. Nothing of it may show: a textarea's own
  // background, caret or focus outline, even nearly transparent, reads as a stray stroke.
  editor.style.cssText='position:absolute;opacity:0.01;width:2px;height:20px;padding:0;border:0;resize:none;pointer-events:none;background:transparent;color:transparent;caret-color:transparent;outline:none;box-shadow:none;appearance:none';
  // The accessibility mirror covers the canvas, each element over the control it
  // stands for, so assistive technology that reads geometry (magnifiers, touch
  // exploration) finds it where it is drawn. It is never seen and never takes
  // the pointer; clipping keeps focus from scrolling it.
  const accessibility=document.createElement('div');accessibility.setAttribute('aria-label','Tilo application');accessibility.style.cssText='position:absolute;top:0;left:0;width:100%;height:100%;overflow:clip;opacity:0;pointer-events:none;';
  const commandStatus=document.createElement('div');commandStatus.setAttribute('role','status');commandStatus.setAttribute('aria-live','polite');
  const status=createHostStatus(commandStatus);
  surface.append(canvas,editor,accessibility,commandStatus);target.append(surface);
  // Shown as HTML (`dom: true`): the frames are built as elements in a layer
  // under the canvas, which stays the input surface but shows nothing. A
  // wheel over one of the layer's scroll containers scrolls it natively
  // (below); `selectText(true)` lets the pointer through to the layer, where
  // the browser's own text selection works on the real text.
  let domPresenter;
  if(options.dom){
    const domLayer=document.createElement('div');domLayer.className='tilo-dom';
    domLayer.style.cssText='position:absolute;inset:0;overflow:hidden;contain:strict;user-select:text;-webkit-user-select:text;';
    canvas.style.cssText+='position:absolute;inset:0;opacity:0;';
    surface.prepend(domLayer);
    domPresenter=createDomPresenter(domLayer,{onFrame:options.onDomFrame,onScroll:(key,offset)=>{
      if(id===undefined||closed)return;
      try{runtime.dom_scrolled(id,key,offset);}catch(error){report({domScroll:String(error)});}
    }});
    globalThis.__tiloDom={stats:domPresenter.stats,layer:domLayer,selectText:on=>{canvas.style.pointerEvents=on?'none':'';}};
  }
  const fillScreen=createScreenFill(surface,{canvas,observe:globalThis.ResizeObserver});
  let id,storage,closed=false,crashed=false,composing=false,pasteShift=false,lastState,readyResolve,readyReject,persistenceFailed=false;
  const presented=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  void presented.catch(()=>{});
  // What the desktop hands over says so where every copy does: the status
  // line, and the page's own status.
  const writeClipboard=text=>globalThis.navigator?.clipboard?.writeText
    ?globalThis.navigator.clipboard.writeText(text)
    :Promise.reject(new Error('this page cannot reach the clipboard'));
  const report=data=>{
    // A finished copy is the app's to show, in its own window; the page only speaks it.
    if(data.clipboard==='copied')status.show('Copied to system clipboard',{spoken:true,again:true});
    // The clipboard says why in a sentence of its own (tilo-clipboard's web/clipboard.mjs).
    if(data.clipboard==='failed')status.show(data.clipboardError||'Could not copy. Try the keyboard Copy shortcut.',{again:true});
    if(data.desktopCommand){const row=data.desktopCommand;status.show(row.state==='succeeded'?'Desktop change saved':row.state==='submitting'||row.state==='accepted'?'Saving desktop change…':row.state==='failed'||row.state==='rejected'?`Desktop change refused: ${row.error??'unavailable'}`:`Desktop change awaiting confirmation: ${row.error??row.state}`);}
    if(data.desktopErrors?.length)status.show(`Desktop request unavailable: ${data.desktopErrors.at(-1)}`);
    options.onStatus?.(data);
  };
  const navigationFocus={errors(){},update(){},close(){}};
  const presentationWaiters=new Set();
  const config={profile:'tilo-browser',device:'tilo-browser-device',workspace:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',author:options.peer?undefined:'Browser user',instance:1,mac:/Mac/.test(navigator.platform),conservativeGpu:isWebKit(navigator),...options};
  // Model credentials are mount-scoped bootstrap input, never a page global.
  // The WASM host binds the authenticated transport before each app factory.
  if(options.models)config.models={endpoint:options.models.endpoint,sessionToken:options.models.sessionToken,profileId:options.models.profileId,modelId:options.models.modelId};
  // Administration is opt-in and never borrows the inference session token.
  if(options.providerManagement)config.providerManagement={endpoint:options.providerManagement.endpoint,sessionToken:options.providerManagement.sessionToken};
  delete config.peer;if(options.peer?.runtimeId!==undefined)config.peer=options.peer.runtimeId;delete config.settings;delete config.onNavigation;delete config.onStatus;delete config.signal;delete config.loadAsset;delete config.subscribeDesktopCommits;delete config.live;delete config.liveMembers;delete config.restartOnCrash;delete config.dom;delete config.onDomFrame;
  // The desktop shows its invite sheet when the page configures one: what it
  // makes links through, as `mountDesktop({invite})` hands it over.
  delete config.onInvite;
  // What only this page can do for the person (keeping their identity): offered in desktop search and on banners.
  delete config.hostActions;delete config.onHostAction;
  // The workspace directory's door granted this person a workspace: only the page joins with it.
  delete config.onJoinGranted;
  let unsubscribeDesktopCommits;
  let here,livePlace=null,livePointer=null;
  // The workspace's live layer as this mount publishes through it (a certified
  // peer's `live` view: only the mount acted in last speaks), else the peer's.
  const liveLayer=()=>typeof options.live==='function'?options.live():options.peer?.live;
  const semanticPresentation=createSemanticPresentation({request:(target,observe)=>{if(active())runtime.request_component_locus(id,JSON.stringify(target),observe);},valid:()=>fontMountValid(),surfaceSize:()=>[canvas.width,canvas.height]});
  const capture=createPresentedSource({audience:JSON.stringify([config.profile,config.device,config.workspace]),app:config.app,canvas,layer:liveLayer,valid:()=>fontMountValid()});
  let media,semantic,semanticKeys,hiddenKeys,composition,navigation,fontDemands,externalFocusEpoch=0,runtimeReady=false,desktopOwnership=false,localActivationGesture;
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
  // Until a resize is drawn the canvas stretches the last frame to its new
  // CSS box, and pointer input maps into that frame the same way; the mirror
  // follows the frame on screen, scaled from its layout size to the canvas.
  // The canvas's CSS size comes from its resize observer, so a frame's state
  // update never forces a synchronous layout to read it.
  let canvasCssSize;
  const placeSemantics=()=>semantic?.layout(canvasCssSize??=[canvas.clientWidth,canvas.clientHeight],lastState?.logicalSize);
  semantic=semanticBridge(accessibility,(...args)=>runtime.semantic(id,...args),report,{
    keydown:(...args)=>{inputScope.capture();return active()&&semanticKeys?.keydown(...args);},
    keyup:(...args)=>{inputScope.capture();return active()&&semanticKeys?.keyup(...args);},
    blur:owner=>{inputScope.capture();if(active())semanticKeys?.blur(owner);},
  },inputScope,domPresenter?.host);
  const listeners=new AbortController();const on=(node,type,fn,opts={})=>node.addEventListener(type,fn,{...opts,signal:listeners.signal});
  // A wheel over a scroll container of the HTML layer moves that container,
  // as on any page: the browser shows the move at once, and the widget hears
  // of it through the container's scroll (`dom_scrolled`). Anything else is
  // the canvas's, which hands it to the runtime.
  if(domPresenter)on(surface,'wheel',event=>{
    if(event.ctrlKey||!active())return;
    const dy=event.deltaMode===1?event.deltaY*40:event.deltaMode===2?event.deltaY*surface.clientHeight:event.deltaY;
    const scroller=dy&&domPresenter.scrollerAt(event.clientX,event.clientY,dy);
    if(!scroller)return;
    event.preventDefault();event.stopPropagation();
    scroller.scrollTop+=dy;
  },{capture:true,passive:false});
  if(globalThis.ResizeObserver){const resized=new ResizeObserver(([entry])=>{canvasCssSize=[entry.contentRect.width,entry.contentRect.height];placeSemantics();});resized.observe(canvas);listeners.signal.addEventListener('abort',()=>resized.disconnect());}
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
  // The pictures people chose for their profiles, worn by their avatars here.
  // The people on the desktop's bar and cards are the desktop's: a page
  // showing one app has none to give a picture, links or a status to.
  async function showPhoto(user,url){if(config.app)return;const pixels=url?await photoPixels(url):null;if(active()&&runtime.desktop_set_person_photo)runtime.desktop_set_person_photo(id,user,pixels?.width??0,pixels?.height??0,pixels?.rgba??new Uint8Array());}
  // What the person says they are doing and how they share it, as their
  // profile keeps it: set on another of their devices, it shows here too.
  function ownStatus(profile){
    // How new the profile this page read is: its presence says so, and a
    // device of theirs with an older one fetches it again.
    options.peer?.setProfileChanged?.(profile.issued_at_ms);
    const status=typeof profile.status==='string'?profile.status:'';
    const sharing=typeof profile.sharing==='string'?profile.sharing:'';
    options.onHostAction?.('set-status',{status,sharing,keep:false});
    // Whether their picture is a linked account's, which a newer one of
    // that account's replaces, or one they uploaded, which stays.
    options.onHostAction?.('set-linked',{photoFrom:typeof profile.photo_from==='string'?profile.photo_from:null,linked:Array.isArray(profile.linked)?profile.linked:[],keep:false});
    if(!config.app&&active()&&runtime.desktop_set_own_status)runtime.desktop_set_own_status(id,status,sharing);
  }
  // The accounts elsewhere someone's profile names, which their card says.
  function showLinked(user,linked){if(!config.app&&active()&&runtime.desktop_set_person_links)runtime.desktop_set_person_links(id,user,JSON.stringify(linked));}
  const photos=followPhotos({profileOf:user=>fetchPersonProfile(options.peer.origin,user),show:showPhoto,linked:showLinked,own:()=>options.peer?.userPublicKey,onOwn:ownStatus,onError:error=>report({photoError:String(error)})});
  const nativeCatalogue = createNativeCatalogueBridge({current:active,
    actions:()=>options.peer?.actions,
    membership:()=>options.peer?.membership,
    take:()=>JSON.parse(runtime.component_catalogue_requests?.(id) ?? '[]'),
    deliver:message=>runtime.component_catalogue_response(id,JSON.stringify(message)),
    onError:error=>report({catalogueError:String(error)})});
  const fontMountValid=()=>!closed&&!crashed&&registry.current(token)&&id!==undefined&&runtimeReady;
  fontDemands=createFontDemandPump({loader:fontLoader,valid:fontMountValid,report,install:(name,bytes)=>runtime.install_font(id,name,bytes,fontLoader.digestOf(bytes)??''),failed:()=>runtime.font_failed(id)});
  function update(raw){
    if(closed||crashed||!registry.current(token))return;
    const state=JSON.parse(raw);if(state.clipboard){report(state);return;}
    // After the first tree a status carries only what changed in it; one
    // made for a tree this page does not hold asks for the whole tree.
    if(state.semanticsPatch){
      const patched=applySemanticsPatch(lastState?.semantics,lastState?.semanticsSeq,state.semanticsPatch);
      if(patched){state.semantics=patched;state.semanticsSeq=state.semanticsPatch.seq;}
      else if(active())queueMicrotask(()=>{if(active())runtime.request_semantics?.(id);});
      delete state.semanticsPatch;
    }
    nativeCatalogue.pump();
    for(const request of state.hostRequests??[]){if(typeof request?.copy==='string')void writeClipboard(request.copy).then(()=>report({clipboard:'copied'}),error=>report({clipboard:'failed',clipboardError:String(error?.message??error)}));else if(request?.action)options.onHostAction?.(request.action);else if(typeof request?.named==='string')options.onHostAction?.('set-name',request.named);else if(request?.status)options.onHostAction?.('set-status',{...request.status,keep:true});else if(Array.isArray(request?.linked))options.onHostAction?.('set-linked',{accounts:request.linked,keep:true});else if(typeof request?.revoke==='string')/* The person takes an agent's grant back: the root this tab holds revokes its device. */options.onHostAction?.('revoke-device',request.revoke);else if(request?.joined)/* The directory's door let this person into a workspace: the page joins with the invitation it granted. */options.onJoinGranted?.(request.joined);else if(typeof request?.open==='string'){/* Signing in elsewhere (Connect Discord) leaves the page for that site, which sends the person back. */if(/^https?:\/\//.test(request.open))location.assign(request.open);}}
    if('focusPresence' in state)liveLayer()?.publishPresence({selection:state.focusPresence});
    if('livePlace' in state){livePlace=state.livePlace;liveLayer()?.publishPresence({focus:livePlace?.focus??null,place:livePlace?.place??null,layout:livePlace?.layout??null});}
    // This person's latest wave, which the one it is for shows once.
    if('liveWave' in state)liveLayer()?.publishPresence({wave:state.liveWave});
    if('liveFollowing' in state)liveLayer()?.publishPresence({following:state.liveFollowing});
    if(!runtimeReady&&['gpu-pending','presented'].includes(state.phase)){
      // A mount of one application holds no desk, so the page answers for its input
      // itself; a desktop's answer is read live (`desktop_input_ownership`).
      desktopOwnership=Boolean(runtime.desktop_navigation_snapshot?.(id));
      runtimeReady=true;
      if(options.hostActions&&runtime.desktop_set_host_actions&&runtime.desktop_navigation_snapshot?.(id))runtime.desktop_set_host_actions(id,JSON.stringify(options.hostActions));
      if(runtime.desktop_navigation_snapshot(id)){
        navigation=createDesktopNavigation({snapshot:()=>runtime.desktop_navigation_snapshot(id),revision:()=>runtime.desktop_navigation_revision(id),dispatch:command=>runtime.desktop_navigate(id,command),active});
        options.onNavigation?.(navigation.controller);
      }
    }
    // The frame's one read of who holds the input: the pumps below and the
    // IME box both use it, and a change it reports has already run.
    const ownership=inputScope.capture(),ime=ownership?.legacy?state.ime:ownership?.ime;
    if(runtimeReady)data.pump(report);
    if(runtimeReady&&options.peer?.voice){media??=createMediaPump({runtime,id,voice:options.peer.voice,workspace:config.workspace,active,report});queueMicrotask(()=>{if(active())media.pump();});}
    // Shaping asks for an optional font rarely, and the take is a wasm call
    // and a decode: ask whether there is anything first.
    if(runtimeReady&&runtime.font_demands&&(!runtime.font_demands_pending||runtime.font_demands_pending(id))){
      const demanded=JSON.parse(runtime.font_demands(id));
      if(demanded.length)void fontDemands(demanded);
    }
    // Search opened from page chrome (a Search button) takes the keyboard.
    if(state.searchOpen&&!lastState?.searchOpen&&![canvas,editor].includes(document.activeElement))canvas.focus({preventScroll:true});
    lastState=retainStatus(lastState,state);report(state);
    if(state.phase==='suspended'){surface.inert=true;editor.disabled=true;}

    if(state.phase==='presented'){surface.inert=false;editor.disabled=false;const layer=liveLayer();if(!livePointer&&layer){livePointer=followOwnPointer(layer,()=>id,surface,active);if(livePlace)layer.publishPresence({focus:livePlace.focus,place:livePlace.place,layout:livePlace.layout??null});}if(!here&&!config.app&&options.peer&&runtime.desktop_say_doing)here=createHere({runtime,mountId:()=>id,active,layer:liveLayer,peer:options.peer,photos:options.peer.origin?photos:null,onError:error=>report({hereError:String(error)})});}
    surface.dataset.phase=state.phase;surface.dataset.frames=String(state.frames??0);
    if(state.phase==='failed'){readyReject(Error(state.error));for(const waiter of presentationWaiters)waiter.reject(Error(state.error));presentationWaiters.clear();}
    if(state.phase==='presented'){for(const waiter of [...presentationWaiters])if((state.frames??0)>waiter.after){presentationWaiters.delete(waiter);waiter.resolve();}}
    navigation?.refresh();
    navigationFocus.errors(state.desktopErrors??[]);
    if(state.phase==='presented'&&navigation)navigationFocus.update(navigation.controller.getSnapshot());
    if(state.semantics)semantic.update(state.semantics);
    placeSemantics();
    fillScreen(!ime);
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
    const proxy=fetchProxyFor(url,{pageHref:location.href});
    const response=proxy
      ?await fetch(...proxiedGet(url,proxy,{signal:listeners.signal}))
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
  // Every presented frame asks to persist, but only a frame that changed
  // something has anything to save. One persist runs at a time: a request
  // arriving while it runs marks `again`, and one more runs after it, so a
  // burst of frames costs one round trip instead of a task per frame.
  let persisting=null,persistAgain=false;
  function persist(){
    if(!active()||!storage||persistenceFailed)return Promise.resolve();
    if(persisting){persistAgain=true;return persisting;}
    persisting=(async()=>{
      try{
        do{
          persistAgain=false;
          if(!active()||persistenceFailed)return;
          await persistOnce();
        }while(persistAgain&&!persistenceFailed);
      }finally{persisting=null;}
    })();
    return persisting;
  }
  async function persistOnce(){
    const current=id;
    // pending_writes also moves the record's unsaved edits into the store.
    const writes=Array.from(runtime.pending_writes(current));
    // Apps' files and stored credentials commit on their own; while neither
    // is waiting, only the records' writes are left to commit.
    const files=runtime.files_dirty?runtime.files_dirty():true;
    try{
      if(files)await runtime.flush_files();
      if(!writes.length&&!files)return;
      await storage.commit(writes);await storage.flush();
      report({persistence:'committed',pendingWrites:0,error:null});
    }catch(error){
      persistenceFailed=true;report({persistence:'failed',pendingWrites:storage.status.pendingWrites,error:String(error)});throw error;
    }
  }
  function onCrash(crash){
    // The module is stopped: nothing on this page may call into it again.
    crashed=true;surface.inert=true;capture.close();semanticPresentation.close();
    const action=crashAction(crash,crash.tab!=null&&crashes.record(config.workspace,crash));
    report({phase:'crashed',crashedWindow:crash.tab??null,error:crash.message,restarting:action==='restart'&&options.restartOnCrash!==false});
    if(action==='restart'&&options.restartOnCrash!==false)setTimeout(()=>location.reload(),250);
  }
  let closeWrites=[];
  let forgetCrashed;
  const cleanup=recoverableClose({
    begin(){
      capture.close();semanticPresentation.close();
      crashListeners.delete(onCrash);
      if(globalThis.__tiloForgetCrashedWindow===forgetCrashed)delete globalThis.__tiloForgetCrashedWindow;
      nativeCatalogue.dispose();media?.close();livePointer?.();livePointer=null;here?.stop();here=undefined;unsubscribeDesktopCommits?.();navigationFocus.close();navigation?.close();semantic.close();closed=true;for(const waiter of presentationWaiters)waiter.reject(Error('mount disposed'));presentationWaiters.clear();listeners.abort();readyReject(Error('mount disposed'));
      if(id!==undefined)closeWrites=Array.from(runtime.close(id));
      surface.remove();
    },
    async settle(){
      // An explicit close waits for every persist outstanding, then saves
      // whatever the mount's own close wrote.
      while(persisting)await persisting.catch(()=>{});
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
    if(!options.dom){
      const refusal=webgpuRefusal();if(refusal)throw Error(refusal);
      if(!navigator.gpu)throw Error('WebGPU is required for the Tilo Vello compute renderer');
    }
    const startupFonts=fonts();void startupFonts.catch(()=>{});
    await ready();if(!registry.current(token))throw Error('mount disposed');
    mark('wasm-ready');
    const acquired=await durableHost.open(config.profile,config.device,{settings:options.settings??browserSettings()});
    mark('storage');
    if(closed||!registry.current(token)){await acquired.release();throw Error("mount disposed");}
    storage=acquired;
    // Apps' own files (OPFS) are mirrored before any app reads them.
    const [assets,entries]=await Promise.all([startupFonts,storage.hydrate(),runtime.mirror_files(config.profile)]);
    mark('assets');
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
    runtime.quarantine_windows?.(JSON.stringify(crashes.entries(config.workspace)));
    // A crashed window the person reopens leaves the record, so the next
    // load runs its app too.
    forgetCrashed=tab=>crashes.forget(config.workspace,tab);
    globalThis.__tiloForgetCrashedWindow=forgetCrashed;
    crashListeners.add(onCrash);
    // Each startup font goes in with the digest the loader computed natively,
    // which the runtime checks against its build instead of hashing again.
    for(const [name,bytes] of Object.entries(assets))runtime.provide_startup_font(name,bytes,fontLoader.digestOf(bytes)??'');
    if(domPresenter)runtime.present_as_dom(domPresenter.present);
    id=runtime.mount(canvas,JSON.stringify(config),entries,raw=>{
      // Pin pixels and geometry while this presentation is still current,
      // before another presentation or a queued page callback can replace it.
      // This path reads only JS/Canvas state, never re-enters the WASM mount.
      const state=JSON.parse(raw);
      if(state.phase){if(state.presentation===true){state.observedPresentedAtMs=performance.now();mark('first-frame');}semanticPresentation.present(state);capture.present(state);}
      queueMicrotask(()=>update(raw));
    });
    mark('mounted');
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
        try{if(kind==='paste'&&!event.clipboardData.getData('text/plain')){event.preventDefault();
            // A picture's bytes are only readable after the event: read them,
            // then dispatch the paste again carrying them.
            const picture=pastedPicture(event.clipboardData);
            if(!picture){report({clipboard:'empty or unsupported clipboard payload'});return;}
            const shift=pasteShift;
            picture.arrayBuffer().then(buffer=>{
              if(!inputScope.valid(scope))return;
              const completed=runtime.clipboard_event(id,'paste',{getData:()=>'',tiloImage:{type:picture.type,bytes:new Uint8Array(buffer)}},shift);
              report({lastInput:kind,clipboard:completed?'picture pasted':'unavailable or no selection'});
            }).catch(error=>report({clipboard:'failed',error:String(error)}));
            return;}
          if(kind==='paste'&&oversizedInsert(event.clipboardData.getData('text/plain'))){event.preventDefault();report({clipboard:'refused',error:`Paste is too large (more than ${MAX_TEXT_INSERT.toLocaleString()} characters); nothing was inserted.`});return;}const completed=runtime.clipboard_event(id,kind,event.clipboardData,pasteShift);event.preventDefault();report({lastInput:kind,clipboard:completed?'completed in trusted DOM event':'unavailable or no selection'});}
        catch(error){report({clipboard:'failed',error:String(error)});}
      });
    }
    // Files dropped on the page go to what is under the pointer, with their
    // bytes: a page's files have no path to open.
    on(canvas,'dragover',event=>{if(!active()||!draggingFiles(event.dataTransfer))return;event.preventDefault();event.dataTransfer.dropEffect='copy';});
    on(canvas,'drop',event=>{
      if(!active()||!draggingFiles(event.dataTransfer))return;
      event.preventDefault();
      const rect=canvas.getBoundingClientRect();
      const x=(event.clientX-rect.left)*(canvas.width/(rect.width||1)),y=(event.clientY-rect.top)*(canvas.height/(rect.height||1));
      droppedFiles(event.dataTransfer).then(files=>{
        if(!files.length)return;
        const handed=runtime.drop_files(id,files,x,y);
        report({lastInput:'drop',drop:`${handed} of ${files.length} files handed over`});
      }).catch(error=>report({drop:'failed',error:String(error)}));
    });
    const dispatchKey=(type,event)=>canvas.dispatchEvent(new KeyboardEvent(type,event));
    hiddenKeys=createKeyboardForwarder({route:keyboardRoute,mac:config.mac,dispatch:dispatchKey,clearModifiers,report:()=>report({lastInput:'keyboard'})});
    semanticKeys=createKeyboardForwarder({route:(...args)=>overlayKeyboardRoute(retainedWidget(lastState),...args),mac:config.mac,dispatch:dispatchKey,publishModifiers,clearModifiers,report:()=>report({lastInput:'keyboard'})});

    const hiddenKeyOwner={};
    const dispatchCanvasKey=(key,mods={})=>{canvas.dispatchEvent(new KeyboardEvent('keydown',{key,...mods}));canvas.dispatchEvent(new KeyboardEvent('keyup',{key,...mods}));};
    // Canvas clicks are hit-tested by the runtime. Page chrome is outside that
    // surface, so route dismissal to the current search owner without stealing
    // focus or consuming the external control's pointer event. A press beside
    // the card closes the desk's search on the desk itself; closing it is what
    // this press means there too.
    on(document,'pointerdown',event=>{
      if(!active()||surface.contains(event.target))return;
      if(inputScope.capture()?.holder==='system-search')runtime.desktop_search_close(id);
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
    // The pointer a wheel names, which the runtime routes the wheel by: a
    // swipe over a window's grid is that grid's, not whatever the pointer was
    // left on.
    const wheelPointer={x:NaN,y:NaN};
    const placePointerAtWheel=event=>{
      if(typeof PointerEvent!=='function')return;
      const move=wheelPointerMove(event,wheelPointer);
      if(move)canvas.dispatchEvent(new PointerEvent('pointermove',move));
    };
    on(surface,'wheel',event=>{
      if(event.target!==canvas)return;
      const lines=wheelNotchesAsLines(event,config.mac,wheelNotch);
      if(!lines)return;
      event.preventDefault();event.stopImmediatePropagation();
      placePointerAtWheel(event);
      canvas.dispatchEvent(new WheelEvent('wheel',lines));
    },{capture:true,passive:false});
    on(canvas,'wheel',event=>{
      event.preventDefault();
      placePointerAtWheel(event);
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
      setHostActions:actions=>{if(active()&&!config.app&&runtime.desktop_set_host_actions)runtime.desktop_set_host_actions(id,JSON.stringify(actions));},
      offerHostAction:offer=>{if(active()&&!config.app&&runtime.desktop_offer_host_action)runtime.desktop_offer_host_action(id,JSON.stringify(offer));},
      // The picture `user` (root key, hex) chose, as a `data:` URL, shown on their avatars now; `null` takes it away.
      setPersonPhoto:(user,url)=>{photos.chose(user,url);return showPhoto(user,url);},
      // On a desktop with no workspace (the person's own), who it shows as its person (`{user, label}`): a workspace's peer names its own.
      setOwnPerson:({user,label})=>{if(active()&&!config.app&&!options.peer&&runtime.desktop_set_people)runtime.desktop_set_people(id,JSON.stringify({own:{publisher:'own',kind:'person',label:String(label??''),user}}));},
      // What the page just did out of sight, said in passing ("Link copied").
      acknowledge:text=>{if(active()&&!config.app&&runtime.desktop_acknowledge)runtime.desktop_acknowledge(id,String(text));},
      // Who the desktop shows as here (`[{id, label, agent}]`, one row per person), or null for a single app.
      people:()=>{const shown=active()&&!config.app&&runtime.desktop_here_people?.(id);return shown?JSON.parse(shown):null;},
      // What the workspace's live layer says of itself: whether it is live,
      // this page's publisher in it, the members it reads, and the publisher's
      // own counters and last publish error (`{state, publisher, members,
      // stats}`), or null for a mount with no layer.
      live:()=>{const layer=liveLayer();return layer?{state:layer.state,publisher:layer.publisherId,members:layer.members(),stats:layer.stats?.()}:null;},
      // What the workspace's call says of itself here: its phase, whether this
      // page's microphone runs, and what left and arrived over the plane
      // (`{phase, captureActive, counters}`) — for tests and tools reading
      // that a call's audio went both ways, or null for a page with no call
      // lane.
      call:()=>options.peer?.voice?.call.stats()??null,
      // Name the workspace's people by the id their actions record (`[{id, name}]`), for a page that knows who it admitted.
      setPeople:people=>{if(active())runtime.set_people(id,JSON.stringify(people));},

      durableState:()=>storage?.hydrate(),failNextWrite:()=>{storage.lease.abortNext=true;},recover:()=>runtime.recover(id),
      semantic:(...args)=>runtime.semantic(id,...args),
      publications:()=>JSON.parse(runtime.component_publications(id)),
      actions:(resource,{window}={})=>JSON.parse(runtime.component_actions(id,resource??undefined,window)),
      // The workspace's own membership actions are the host's, not an app's: a promise of the host's answer.
      invokeAction:(resource,action,args={},{confirm=false,window}={})=>invokeMembershipAction({membership:options.peer?.membership,workspace:options.peer?.workspace,resource,action,args,confirm})
        ??JSON.parse(runtime.invoke_component_action(id,resource,action,JSON.stringify(args),confirm,window)),};
  }catch(error){registry.remove(token);await cleanup();throw error;}
}
async function settleBeforeGpu(){
  if(document.readyState!=='complete')await new Promise(resolve=>addEventListener('load',resolve,{once:true}));
  await new Promise(resolve=>setTimeout(resolve,1500));
  await new Promise(resolve=>(globalThis.requestIdleCallback??(f=>setTimeout(f,0)))(resolve,{timeout:1500}));
}
// This person's pointer, said over the tab it is on: a point in that tab's
// logical pixels, which every member's view of the tab places. The session's
// roster draws the members' pointers from the board; this only says where this
// page's is.
function followOwnPointer(layer,mountId,surface,active){
  const move=event=>{
    if(!active())return;
    const rect=surface.getBoundingClientRect();
    const at=runtime.pointer_tile(mountId(),event.clientX-rect.left,event.clientY-rect.top);
    layer.publishPresence({pointer:at?JSON.parse(at):null});
  };
  const leave=()=>layer.publishPresence({pointer:null});
  surface.addEventListener('pointermove',move,{passive:true});
  surface.addEventListener('pointerleave',leave,{passive:true});
  return ()=>{surface.removeEventListener('pointermove',move);surface.removeEventListener('pointerleave',leave);};
}
export function pendingRecoveryCount(){return recoveryJobs.size;}
export async function retryPendingRecovery(){for(const retry of [...recoveryJobs])await retry();}
export async function shutdown(){await retryPendingRecovery();registry.shutdown();await Promise.all([...closures].map(close=>close()));await new Promise(resolve=>runtime.shutdown(resolve));registry=new MountRegistry();}
