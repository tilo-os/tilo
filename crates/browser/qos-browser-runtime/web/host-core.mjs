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
const applicationCommands=new Set(['a','b','f','i','u','y','z']);
const clipboardCommands=new Set(['c','v','x']);
export function keyboardRoute(event,composing,mac) {
  if(composing||event.isComposing)return 'composition';
  const key=String(event.key??'');
  const lower=key.toLowerCase();
  const primary=mac?Boolean(event.metaKey):Boolean(event.ctrlKey);
  if(primary&&clipboardCommands.has(lower))return 'clipboard';
  if(primary&&!event.altKey&&applicationCommands.has(lower))return 'forward';
  const altGraph=Boolean(event.ctrlKey&&event.altKey&&!event.metaKey);
  if(key.length===1&&(!primary||altGraph))return 'text';
  if(primary||event.metaKey||event.altKey)return 'browser';
  if(['Alt','Control','Meta','Shift'].includes(key))return 'modifier';
  return 'forward';
}
export function modifierSnapshot(event) {
  return [Boolean(event.ctrlKey),Boolean(event.altKey),Boolean(event.shiftKey),Boolean(event.metaKey)];
}
export function beforeInputRoute(event,composing) {
  if(composing||event.isComposing)return {kind:'ignore'};
  if(['insertFromPaste','insertCompositionText'].includes(event.inputType))return {kind:'handled-elsewhere'};
  if(event.inputType==='insertText'&&event.data)return {kind:'commit',text:event.data};
  if(['insertLineBreak','insertParagraph'].includes(event.inputType))return {kind:'key',key:'Enter'};
  if(event.inputType==='deleteContentBackward')return {kind:'key',key:'Backspace'};
  if(event.inputType==='deleteContentForward')return {kind:'key',key:'Delete'};
  return {kind:'ignore'};
}
export function transactionComplete(transaction) {
  return new Promise((resolve,reject) => {
    transaction.addEventListener('complete',resolve,{once:true});
    transaction.addEventListener('abort',() => reject(transaction.error ?? Error('IndexedDB transaction aborted')),{once:true});
    transaction.addEventListener('error',() => reject(transaction.error ?? Error('IndexedDB transaction failed')),{once:true});
  });
}
