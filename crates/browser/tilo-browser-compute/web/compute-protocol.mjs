// Correlation only: host authorization and the Rust completion ticket remain authoritative.
export const VERSION = 1;
export const CHUNK = 256 * 1024;
const apply = Reflect.apply;
const Bytes = Uint8Array;
const Buffer = ArrayBuffer;
const typed = Object.getPrototypeOf(Bytes.prototype);
const getter = key => Object.getOwnPropertyDescriptor(typed, key).get;
const backing = getter('buffer'), offset = getter('byteOffset'), length = getter('byteLength');
const tag = Object.getOwnPropertyDescriptor(typed, Symbol.toStringTag).get;
const bufferLength = Object.getOwnPropertyDescriptor(Buffer.prototype, 'byteLength').get;
const set = Bytes.prototype.set;
export const fault = code => new Error(`image compute ${code}`);
export function inputView(value) {
  try {
    if (apply(tag,value,[]) !== 'Uint8Array') throw fault('protocol');
    const buffer=apply(backing,value,[]), start=apply(offset,value,[]), size=apply(length,value,[]);
    apply(bufferLength,buffer,[]); // rejects shared memory
    return new Bytes(buffer,start,size); // rejects detached memory
  } catch { throw fault('protocol'); }
}
export function bufferView(value) {
  try { return new Bytes(value,0,apply(bufferLength,value,[])); } catch { throw fault('protocol'); }
}
export function copyChunk(target,source,start,size) {
  apply(set,target,[new Bytes(apply(backing,source,[]),apply(offset,source,[])+start,size),start]);
}
export function fields(value) {
  if (!value || typeof value!=='object' || ![Object.prototype,null].includes(Object.getPrototypeOf(value))) throw fault('protocol');
  const result=Object.create(null);
  for(const key of Reflect.ownKeys(value)) {
    const d=Object.getOwnPropertyDescriptor(value,key);
    if(typeof key!=='string' || !Object.hasOwn(d,'value')) throw fault('protocol');
    result[key]=d.value;
  }
  return result;
}
export function matches(reply,id) {
  return ['version','owner','lifetime','domain','request','source'].every(k=>reply[k]===id[k]);
}
export function taskYield() {
  return new Promise(resolve=>{
    const channel=new MessageChannel();
    channel.port1.onmessage=()=>{channel.port1.close();channel.port2.close();resolve();};
    channel.port2.postMessage(null);
  });
}
