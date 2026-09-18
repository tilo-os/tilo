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
export function transactionComplete(transaction) {
  return new Promise((resolve,reject) => {
    transaction.addEventListener('complete',resolve,{once:true});
    transaction.addEventListener('abort',() => reject(transaction.error ?? Error('IndexedDB transaction aborted')),{once:true});
    transaction.addEventListener('error',() => reject(transaction.error ?? Error('IndexedDB transaction failed')),{once:true});
  });
}
