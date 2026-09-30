const hex=bytes=>[...new Uint8Array(bytes)].map(value=>value.toString(16).padStart(2,'0')).join('');
const NAME=/^[A-Za-z0-9][A-Za-z0-9._-]*(\/[A-Za-z0-9][A-Za-z0-9._-]*)*$/;

/**
 * Answers the runtime's `ui::host_data` requests from the bundle: `data.json`
 * names each file under `data/` with its size and SHA-256. A name the
 * manifest does not list, a failed fetch or a mismatch is reported to the
 * runtime as a failure; nothing unverified is installed. A bundle without
 * `data.json` fails every request.
 */
export function createDataLoader({baseURL,runtime,fetch:fetchAsset=globalThis.fetch,crypto=globalThis.crypto}){
  const root=new URL(baseURL);
  const failed=new Set();
  let manifest;
  async function fetchRequired(url){
    const response=await fetchAsset(url,{credentials:'same-origin',mode:'same-origin',redirect:'error'});
    if(!response.ok)throw Error(`data HTTP ${response.status}`);
    return response;
  }
  function assets(){
    manifest??=fetchRequired(new URL('./data.json',root)).then(response=>response.json()).then(value=>{
      if(value?.schema!==1||!Array.isArray(value.assets))throw Error('invalid data manifest');
      const byName=new Map();
      for(const asset of value.assets){
        if(!asset||typeof asset.name!=='string'||!NAME.test(asset.name)||!Number.isSafeInteger(asset.bytes)||asset.bytes<0||
          !/^[0-9a-f]{64}$/.test(asset.sha256)||byName.has(asset.name))throw Error('invalid data manifest');
        byName.set(asset.name,asset);
      }
      return byName;
    });
    manifest.catch(()=>{manifest=undefined;});
    return manifest;
  }
  async function load(name){
    const asset=(await assets()).get(name);
    if(!asset)throw Error(`${name} is not in this bundle`);
    const bytes=new Uint8Array(await (await fetchRequired(new URL(`./data/${name}`,root))).arrayBuffer());
    if(bytes.byteLength!==asset.bytes||hex(await crypto.subtle.digest('SHA-256',bytes))!==asset.sha256)throw Error(`${name} failed verification`);
    return bytes;
  }
  function install(name,report){
    return load(name).then(bytes=>{
      runtime.install_data(name,bytes);
      if(failed.delete(name))report?.({data:'available',dataAsset:name});
    },error=>{
      failed.add(name);
      runtime.data_failed(name,String(error?.message??error));
      report?.({data:'unavailable',dataAsset:name,error:String(error?.message??error)});
      throw error;
    });
  }
  /** Fetch whatever the runtime asked for since the last pump; `report`
   * hears `{data:'unavailable'|'available',dataAsset}`. */
  function pump(report){
    if(!runtime.data_requests)return;
    for(const name of JSON.parse(runtime.data_requests()))void install(name,report).catch(()=>{});
  }
  /** Fetch and verify a failed file again; rejects while it stays unavailable. */
  function retry(name,report){
    if(!failed.has(name))return Promise.reject(Error(`${name} has not failed`));
    return install(name,report);
  }
  return {pump,retry};
}
