// A mount's `open` option: a file handed to the app as an explicit input.
// The bytes arrive through the page's admitted path (`load`); the runtime
// delivers them to the component, which declares what it opens.

/** The largest file a page may hand an app (the SDK's `MAX_OPEN_FILE_BYTES`). */
export const MAX_OPEN_FILE_BYTES=128*1024*1024;
const BY_EXTENSION=new Map([['epub','application/epub+zip']]);

function openError(code,message){return Object.assign(Error(message),{code});}
function fileName(url){
  const segment=url.pathname.split('/').pop()??'';
  try{return decodeURIComponent(segment);}catch{return segment;}
}
function asBytes(bytes){
  if(bytes instanceof Uint8Array)return bytes;
  if(bytes instanceof ArrayBuffer)return new Uint8Array(bytes);
  throw openError('TILO_OPEN_INVALID','open.bytes must be a Uint8Array or an ArrayBuffer');
}

/**
 * Normalise `open` ({url | bytes, name?, mediaType?}) against the page's
 * `base` URL: `{url?, bytes?, name, mediaType}` with a lowercase
 * `type/subtype`. Throws `code: 'TILO_OPEN_INVALID'`.
 */
export function openRequest(open,base){
  if(!open||typeof open!=='object')throw openError('TILO_OPEN_INVALID','open must be {url, name?, mediaType?} or {bytes, name, mediaType?}');
  const hasUrl=open.url!==undefined,hasBytes=open.bytes!==undefined;
  if(hasUrl===hasBytes)throw openError('TILO_OPEN_INVALID','open needs exactly one of url or bytes');
  let url;
  if(hasUrl){try{url=new URL(String(open.url),base);}catch{throw openError('TILO_OPEN_INVALID',`open.url is not a URL: ${open.url}`);}}
  const name=String(open.name??(url?fileName(url):'')).trim();
  if(!name)throw openError('TILO_OPEN_INVALID','open needs a name');
  const extension=name.includes('.')?name.split('.').pop().toLowerCase():'';
  const mediaType=String(open.mediaType??BY_EXTENSION.get(extension)??'').split(';')[0].trim().toLowerCase();
  if(!/^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/.test(mediaType))throw openError('TILO_OPEN_INVALID',`open needs a mediaType for ${name}`);
  return {url,bytes:hasBytes?asBytes(open.bytes):undefined,name,mediaType};
}

/**
 * Refuse a file the mounted application does not open (its `opens`), with
 * `code: 'TILO_OPEN_UNSUPPORTED'`, before any bytes are fetched.
 */
export function checkOpens(application,mediaType){
  const opens=application?.opens??[];
  if(opens.includes(mediaType))return;
  const title=application?.title??'The desktop';
  throw openError('TILO_OPEN_UNSUPPORTED',opens.length?`${title} does not open ${mediaType} files (it opens ${opens.join(', ')})`:`${title} does not open files`);
}

/**
 * The request's bytes: its own, or `load(url)`'s. Throws
 * `code: 'TILO_OPEN_UNAVAILABLE'` when loading fails and
 * `'TILO_OPEN_INVALID'` beyond `MAX_OPEN_FILE_BYTES`.
 */
export async function loadOpenBytes(request,load){
  let bytes=request.bytes;
  if(!bytes){
    try{bytes=asBytes(await load(request.url.href));}
    catch(error){throw openError('TILO_OPEN_UNAVAILABLE',`Could not load ${request.name}: ${error instanceof Error?error.message:String(error)}`);}
  }
  if(bytes.byteLength>MAX_OPEN_FILE_BYTES)throw openError('TILO_OPEN_INVALID',`${request.name} is larger than ${MAX_OPEN_FILE_BYTES/(1024*1024)} MiB`);
  return bytes;
}
