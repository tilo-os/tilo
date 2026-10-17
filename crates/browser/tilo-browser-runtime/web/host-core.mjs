/** Generation ownership survives asynchronous GPU initialization and late cleanup. */
export class MountRegistry {
  #next = 0; #entries = new Map(); #targets = new WeakMap(); #closed = false;
  begin(target) {
    if (this.#closed) throw Error('host shutdown');
    const previous = this.#targets.get(target);
    if (previous) this.remove(previous);
    const token = Object.freeze({id: ++this.#next, target});
    this.#targets.set(target, token); this.#entries.set(token.id, {token});
    return token;
  }
  current(token) { return this.#targets.get(token.target) === token && this.#entries.has(token.id); }
  finish(token, host) {
    if (!this.current(token)) return false;
    this.#entries.get(token.id).host = host; return true;
  }
  remove(token) {
    if (!this.current(token)) return false;
    const entry = this.#entries.get(token.id);
    this.#entries.delete(token.id); this.#targets.delete(token.target);
    entry.host?.close(); return true;
  }
  shutdown() { for (const {token} of this.#entries.values()) this.remove(token); this.#closed = true; }
}
export function physicalViewport(width, height, dpr) {
  if (!Number.isFinite(dpr) || dpr <= 0) throw Error('invalid device pixel ratio');
  return {width: Math.ceil(Math.max(0,width)*dpr), height: Math.ceil(Math.max(0,height)*dpr), dpr, suspended: width<=0 || height<=0};
}
export function wheelPixels(x,y,mode,width,height) {
  return [-x*(mode===1?16:mode===2?width:1), -y*(mode===1?16:mode===2?height:1)];
}
/** The runtime reads pixel and line wheel deltas itself but drops page
 * deltas; a page-mode event is re-sent as the pixels it scrolls, with both
 * axes, the pointer and every modifier. `undefined` for events the runtime
 * reads as they are. */
export function pageWheelAsPixels(event,width,height) {
  if(event.deltaMode!==2)return undefined;
  const [x,y]=wheelPixels(event.deltaX,event.deltaY,2,width,height);
  return {deltaX:-x,deltaY:-y,deltaMode:0,clientX:event.clientX,clientY:event.clientY,
    ctrlKey:event.ctrlKey,shiftKey:event.shiftKey,altKey:event.altKey,metaKey:event.metaKey,bubbles:false,cancelable:true};
}
/** A mouse wheel's notches as the line deltas the native session reads from
 * a wheel (libinput's wheel source, one line a notch), so a sideways notch
 * pans the strip a line (40 px) and a vertical one scrolls by a line, as
 * natively, whichever browser it came through. `undefined` for everything
 * the runtime reads as it is: a trackpad's or a Mac mouse's continuous
 * pixels, page deltas, and events the page made itself.
 *
 * Chrome and Safari report pixels for both kinds of device; a notch is told
 * apart by its legacy `wheelDelta`, a multiple of 120 a notch, which a
 * trackpad's never is beside its pixels (it is their -3 multiple there), and
 * by moving at least about a line's worth of pixels a notch. Chromium sums
 * the ticks along with the deltas when it merges queued wheel events
 * (`WebMouseWheelEvent::Coalesce`), so a merged event's `wheelDelta` counts
 * every notch in it, as many as its pixels do.
 *
 * Firefox reports a notch as several lines (six sideways, three down, by
 * default), and only to a reader that asks for `deltaMode` first, so this
 * must run before the runtime's own listener. Its `wheelDelta` is the
 * notch's ticks times 120 (`WheelEvent::WheelDeltaX`), but when its content
 * process merges queued events it sums their line deltas and keeps the
 * first one's ticks (`CoalescedWheelData::Coalesce`), so a merged event's
 * `wheelDelta` undercounts. The lines a notch moves are therefore learnt in
 * `perNotch`, per axis, as the fewest seen per 120 of `wheelDelta`, and an
 * event's lines are divided by them: three merged notches are three lines.
 *
 * Off a Mac, Chrome turns Shift+wheel into a sideways wheel; natively
 * Shift+wheel stays vertical with Shift held, which content reads as
 * sideways scrolling but the strip does not take as a sideways step, so it
 * is turned back. */
export function wheelNotchesAsLines(event,mac,perNotch={x:0,y:0}) {
  if(!event.isTrusted)return undefined;
  const mode=event.deltaMode;
  if(mode!==0&&mode!==1)return undefined;
  const wx=Number(event.wheelDeltaX)||0,wy=Number(event.wheelDeltaY)||0;
  const dx=Number(event.deltaX)||0,dy=Number(event.deltaY)||0;
  let x,y;
  if(mode===1){
    [x,y]=[notchLines(dx,wx,perNotch,'x'),notchLines(dy,wy,perNotch,'y')];
  }else{
    if((wx===0&&wy===0)||wx%120!==0||wy%120!==0)return undefined;
    if(Math.abs(wx)===Math.abs(3*dx)&&Math.abs(wy)===Math.abs(3*dy))return undefined;
    const notches=Math.max(Math.abs(wx),Math.abs(wy))/120,pixels=Math.max(Math.abs(dx),Math.abs(dy));
    if(pixels/notches<20)return undefined;
    [x,y]=[-wx/120||0,-wy/120||0];
  }
  if(!mac&&event.shiftKey&&y===0&&x!==0)[x,y]=[0,x];
  return {deltaX:x,deltaY:y,deltaMode:1,clientX:event.clientX,clientY:event.clientY,
    ctrlKey:event.ctrlKey,shiftKey:event.shiftKey,altKey:event.altKey,metaKey:event.metaKey,bubbles:false,cancelable:true};
}
/** The lines Firefox moves a notch by default, for an event that does not
 * say through its `wheelDelta`. */
const FIREFOX_NOTCH_LINES={x:6,y:3};
function notchLines(lines,wheelDelta,perNotch,axis) {
  if(lines===0)return 0;
  if(wheelDelta!==0){
    const seen=Math.abs(lines)/(Math.abs(wheelDelta)/120);
    if(!(perNotch[axis]>0)||seen<perNotch[axis])perNotch[axis]=seen;
  }
  return lines/(perNotch[axis]>0?perNotch[axis]:FIREFOX_NOTCH_LINES[axis]);
}
const applicationCommands=new Set(['a','b','f','i','k','u','y','z']);
const clipboardCommands=new Set(['c','v','x']);
/** The keys that move the caret or delete in text: with the word modifier
 * (Ctrl, Option on a Mac) or the line modifier (Cmd on a Mac) they still
 * edit text, so they are the app's, not the browser's. */
const textEditingKeys=new Set(['Backspace','Delete','ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End']);
/** Whether `event` is a modified text-editing key: Ctrl+Backspace, Ctrl+Shift+Left
 * and the like; on a Mac, Option or Cmd with them. Alt+Left/Right off a Mac
 * stay the browser's history keys. */
function isModifiedTextEdit(event,mac) {
  if(!textEditingKeys.has(String(event.key??'')))return false;
  if(mac)return Boolean(event.altKey||event.metaKey)&&!event.ctrlKey;
  return Boolean(event.ctrlKey)&&!event.altKey&&!event.metaKey;
}
const desktopChordKeys=new Set(['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','PageUp','PageDown','KeyO',
  'Digit1','Digit2','Digit3','Digit4','Digit5','Digit6','Digit7','Digit8','Digit9']);
/** The desktop's own chords (the browser shell's `chord`): Super with a
 * navigation key, or the page's stand-in for Super, Alt+Shift (Ctrl+Option
 * on a Mac). They belong to the desktop wherever the keyboard is in the
 * surface, a focused text field included, as Super chords do natively. */
export function isDesktopChord(event,mac) {
  if(event.isComposing)return false;
  const native=Boolean(event.metaKey)&&!event.altKey&&!event.ctrlKey;
  const standIn=!event.metaKey&&Boolean(event.altKey)&&(mac?Boolean(event.ctrlKey)&&!event.shiftKey:Boolean(event.shiftKey)&&!event.ctrlKey);
  if(!native&&!standIn)return false;
  if(native&&(event.key==='Tab'||event.code==='Tab'))return true;
  return desktopChordKeys.has(event.code)||desktopChordKeys.has(event.key)||/^[1-9]$/.test(String(event.key??''));
}
export function keyboardRoute(event,composing,mac) {
  if(composing||event.isComposing)return 'composition';
  if(isDesktopChord(event,mac))return 'forward';
  const key=String(event.key??'');
  const lower=key.toLowerCase();
  const primary=mac?Boolean(event.metaKey):Boolean(event.ctrlKey);
  if(primary&&clipboardCommands.has(lower))return 'clipboard';
  if(primary&&!event.altKey&&applicationCommands.has(lower))return 'forward';
  const altGraph=Boolean(event.ctrlKey&&event.altKey&&!event.metaKey);
  if(key==='Dead')return 'text';
  if(key.length===1&&(!primary||altGraph))return 'text';
  if(isModifiedTextEdit(event,mac))return 'forward';
  if(primary||event.metaKey||event.altKey)return 'browser';
  if(['Alt','Control','Meta','Shift'].includes(key))return 'modifier';
  return 'forward';
}
export function modifierSnapshot(event) {
  return [Boolean(event.ctrlKey),Boolean(event.altKey),Boolean(event.shiftKey),Boolean(event.metaKey)];
}
export function semanticKeyboardRoute(event,composing,mac) {
  if(keyboardRoute(event,composing,mac)!=='forward')return 'native';
  if(isDesktopChord(event,mac))return 'forward';
  const key=String(event.key??'').toLowerCase();
  if(key==='enter'&&!event.ctrlKey&&!event.altKey&&!event.metaKey)return 'forward';
  if(key==='k')return 'forward';
  if(mac)return key==='z'&&event.metaKey&&!event.ctrlKey&&!event.altKey?'forward':'native';
  if(!event.ctrlKey||event.metaKey||event.altKey)return 'native';
  return key==='z'||(key==='y'&&!event.shiftKey)?'forward':'native';
}
export function createKeyboardForwarder({route,dispatch,publishModifiers=()=>{},clearModifiers=()=>{},report=()=>{},mac=false}) {
  const forwarded=new Map();let currentOwner;
  const keyOf=event=>event.code||event.key;
  const releaseEvent=event=>({key:event.key,code:event.code,ctrlKey:false,altKey:false,shiftKey:false,metaKey:false});
  function release(owner) {
    let released=false;
    for(const [key,entry] of forwarded){
      if(entry.owner!==owner)continue;
      dispatch('keyup',entry.event);forwarded.delete(key);released=true;
    }
    if(currentOwner!==owner&&!released)return false;
    if(currentOwner===owner)currentOwner=undefined;
    clearModifiers();return true;
  }
  return {
    keydown(owner,event,composing,platformMac=mac,authorize=()=>true) {
      if(currentOwner!==owner){if(currentOwner!==undefined)release(currentOwner);currentOwner=owner;}
      publishModifiers(event);
      if(route(event,composing,platformMac)!=='forward')return false;
      event.preventDefault();
      if(!authorize()){release(owner);return false;}
      dispatch('keydown',event);forwarded.set(keyOf(event),{owner,event:releaseEvent(event)});report();return true;
    },
    keyup(owner,event) {
      const key=keyOf(event);const entry=forwarded.get(key);
      if(currentOwner!==owner&&entry?.owner!==owner)return false;
      if(currentOwner===owner)publishModifiers(event);
      if(entry?.owner!==owner)return false;
      forwarded.delete(key);event.preventDefault();dispatch('keyup',event);return true;
    },
    blur:release,
    invalidate(){forwarded.clear();currentOwner=undefined;},
  };
}
/** Largest single text insertion (paste or programmatic insertText) the runtime
 * accepts, in UTF-16 units. Larger inserts are refused with a visible status
 * instead of stalling or exhausting the page (layout, scene and publication
 * all scale with it). */
export const MAX_TEXT_INSERT=1_000_000;
export function oversizedInsert(text){return typeof text==='string'&&text.length>MAX_TEXT_INSERT;}
export function beforeInputRoute(event,composing) {
  if(composing||event.isComposing)return {kind:'ignore'};
  if(['insertFromPaste','insertCompositionText'].includes(event.inputType))return {kind:'handled-elsewhere'};
  if(event.inputType==='insertText'&&event.data&&oversizedInsert(event.data))return {kind:'refuse',length:event.data.length};
  if(event.inputType==='insertText'&&event.data)return {kind:'commit',text:event.data};
  if(['insertLineBreak','insertParagraph'].includes(event.inputType))return {kind:'key',key:'Enter'};
  if(event.inputType==='deleteContentBackward')return {kind:'key',key:'Backspace'};
  if(event.inputType==='deleteContentForward')return {kind:'key',key:'Delete'};
  if(event.inputType==='deleteWordBackward')return {kind:'key',key:'Backspace',ctrlKey:true};
  if(event.inputType==='deleteWordForward')return {kind:'key',key:'Delete',ctrlKey:true};
  return {kind:'ignore'};
}
export function transactionComplete(transaction) {
  return new Promise((resolve,reject) => {
    transaction.addEventListener('complete',resolve,{once:true});
    transaction.addEventListener('abort',() => reject(transaction.error ?? Error('IndexedDB transaction aborted')),{once:true});
    transaction.addEventListener('error',() => reject(transaction.error ?? Error('IndexedDB transaction failed')),{once:true});
  });
}

/** Desktop search's chords, as the native shell's: Super+K and Super+Space
 * (Cmd on a Mac), and Ctrl+K where Ctrl is the command key. */
export function isDesktopSearchShortcut(event,mac) {
  if(event.repeat||event.isComposing||event.altKey||event.shiftKey)return false;
  const key=String(event.key??'').toLowerCase();
  if(key==='k'||event.code==='KeyK')return Boolean(event.metaKey)||(!mac&&Boolean(event.ctrlKey)&&!event.metaKey);
  if(key===' '||event.code==='Space')return Boolean(event.metaKey)&&!event.ctrlKey;
  return false;
}
/** Consume the chord only once the mounted desktop has accepted Search. */
export function routeGlobalSearchShortcut(event,composing,open) {
  if(event.defaultPrevented||event.repeat||composing||event.isComposing||event.ctrlKey||event.shiftKey)return false;
  if(event.key!==' '&&event.code!=='Space')return false;
  if(Boolean(event.altKey)===Boolean(event.metaKey))return false;
  try{open();}catch{return false;}
  event.preventDefault();event.stopImmediatePropagation();return true;
}
export function overlayKeyboardRoute(widget,...args) {
  return widget==='ActionPalette'||widget==='SystemSearch'?keyboardRoute(...args):semanticKeyboardRoute(...args);
}
/** Statuses omit the semantics tree when it is unchanged or throttled; the retained tree stays authoritative until a status carries a new one. */
export function retainStatus(previous,next){return {...previous,...next};}
export function retainedWidget(state){return state?.semantics?.root?.widget;}

/** A capability belongs to one mount and one authoritative runtime input sequence. */
export function createInputScope({read,active,onChange=()=>{}}) {
  let mount={},previous;
  function capture(){
    if(!active()){if(previous){previous=undefined;mount={};onChange(null);}return null;}
    const raw=read?.();
    const value=raw?JSON.parse(raw):{sequence:'legacy',activation:'legacy',owner:null,legacy:true};
    if(typeof value.sequence!=='string'||typeof value.activation!=='string'||!(value.owner===null||typeof value.owner==='string'))return null;
    const next={...value,mount};
    const changed=previous&&(previous.sequence!==next.sequence||previous.activation!==next.activation||previous.owner!==next.owner);
    previous=next;
    if(changed)onChange(next);
    return next;
  }
  function valid(saved){const next=capture();return Boolean(saved&&next&&saved.mount===mount&&saved.sequence===next.sequence&&saved.activation===next.activation&&saved.owner===next.owner);}
  return {capture,valid,owns(owner){const next=capture();return Boolean(next&&(next.legacy||next.owner===owner));}};
}

/** Only an explicitly observed local activation can queue a browser focus transfer. */
export function createFocusHandoff({scope,epoch,focus}) {
  let serial=0;
  return {complete(before,externalEpoch=epoch()){
    const after=scope.capture();
    if(!before||!after||after.legacy||(before.sequence===after.sequence&&before.activation===after.activation&&before.owner===after.owner))return Promise.resolve(false);
    const request=++serial;
    return Promise.resolve().then(()=>{
      if(request!==serial||epoch()!==externalEpoch||!scope.valid(after))return false;
      focus(scope.capture());return true;
    });
  }};
}

/** A revoked native IME session never expires; the host replaces its DOM editor. */
export function createCompositionSession({scope,send,clear}) {
  let session=null,canceled=false;
  function cancel(){const hadSession=Boolean(session);if(hadSession){session=null;canceled=true;}clear();return hadSession;}
  function valid(){if(session&&!scope.valid(session))cancel();return Boolean(session);}
  return {
    cancel,
    get composing(){return valid();},
    start(){const next=scope.capture();if(!next||(!next.legacy&&next.owner===null))return false;session=next;canceled=false;send('start','');return true;},
    update(text){if(valid())send('preedit',text);},
    end(text){if(!valid()){clear();return false;}send('preedit','');send('commit',text);session=null;canceled=true;clear();return true;},
    blocks(event){valid();return canceled||Boolean(session)||event.isComposing||event.inputType==='insertCompositionText';},
  };
}
