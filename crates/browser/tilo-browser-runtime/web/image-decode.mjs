/** Decode an image's bytes with the browser's own decoders into unpremultiplied
 * RGBA, scaled down to fit `maxDimension` on either side: the page's module
 * carries no image decoders of its own. Runs in the image worker
 * (web/image-decode-worker.mjs), or on the page where it cannot start. */
export async function decodeImage(bytes,maxDimension){
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

/** A decoder that runs `decodeImage` in a worker, so a page of thumbnails
 * is not drawn, scaled and read back on the page's main thread (the first
 * 2D context alone sets up a GPU channel there). Falls back to decoding on
 * the page when the worker cannot start or fails. */
export function createImageDecoder({Worker=globalThis.Worker,url=new URL('./image-decode-worker.mjs',import.meta.url),decode=decodeImage}={}){
  let worker;
  let next=0;
  const pending=new Map();
  function fallBack(){
    worker=null;
    for(const [,request] of pending)decode(request.bytes,request.maxDimension).then(request.resolve,request.reject);
    pending.clear();
  }
  function start(){
    if(worker!==undefined)return worker;
    try{
      worker=new Worker(url,{type:'module',name:'tilo-image-decode'});
    }catch{
      worker=null;
      return worker;
    }
    worker.onmessage=({data})=>{
      const request=pending.get(data.id);
      if(!request)return;
      pending.delete(data.id);
      if(data.image)request.resolve(data.image);
      else request.reject(Error(data.error));
    };
    worker.onerror=event=>{event?.preventDefault?.();worker?.terminate?.();fallBack();};
    return worker;
  }
  return (bytes,maxDimension)=>{
    if(!start())return decode(bytes,maxDimension);
    return new Promise((resolve,reject)=>{
      const id=next++;
      pending.set(id,{bytes,maxDimension,resolve,reject});
      worker.postMessage({id,bytes,maxDimension});
    });
  };
}
