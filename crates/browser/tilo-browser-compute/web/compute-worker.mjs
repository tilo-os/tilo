import init, {decode_image} from './pkg/compute/tilo_browser_compute.js';
import {VERSION,CHUNK,fields,bufferView} from './compute-protocol.mjs';
let identity, pixels, offset=0, busy=false;
// Exclusive worker: no request reuse or caller-supplied URL/module/fetch operation.
self.onmessage=async event=>{
  try {
    const m=fields(event.data);
    if(m.type==='decode'){
      if(busy)throw Error('duplicate request');busy=true;
      identity=fields(m.identity);
      if(identity.version!==VERSION||!Number.isSafeInteger(m.maxDimension)||m.maxDimension<1||m.maxDimension>4096)throw Error('protocol');
      const input=bufferView(m.bytes);
      if(input.length>16*1024*1024)throw Error('encoded limit');
      await init();
      const began=performance.now();
      const decoded=decode_image(input,m.maxDimension);
      let width,height;
      try {width=decoded.width;height=decoded.height;pixels=decoded.take_rgba();} finally{decoded.free();}
      self.postMessage({...identity,type:'result',width,height,length:pixels.length,decodeMs:performance.now()-began,decodeStartedAt:performance.timeOrigin+began,decodeFinishedAt:performance.timeOrigin+performance.now(),worker:typeof WorkerGlobalScope!=='undefined'&&self instanceof WorkerGlobalScope});
    }else if(m.type==='credit'){
      if(!pixels||m.offset!==offset||offset>=pixels.length)throw Error('credit');
      const chunk=pixels.slice(offset,Math.min(offset+CHUNK,pixels.length));
      const size=chunk.length;
      self.postMessage({...identity,type:'chunk',offset,bytes:chunk.buffer},[chunk.buffer]);
      offset+=size;
      if(offset===pixels.length)pixels=null;
    }else throw Error('protocol');
  }catch{
    self.postMessage({...identity,type:'failed'});self.close();
  }
};
