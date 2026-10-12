/** Read-only composition of explicitly supplied, presented single-app mounts.
 * No input, document edits, presence publication, discovery or animation loop.
 * A pointer is an observed LIVE_V1 pointer, never inferred from an MCP session.
 */
import {STALE_AFTER_MS} from './live-presence.mjs';
import {colorOf} from './peer.mjs';
import {markerText} from './live-cursors.mjs';

const sources = new WeakMap();
const sourceCanvases = new WeakSet();
export const MAX_PANES = 20;
export const MAX_SOURCE_PIXELS = 2_097_152;
export const MAX_OUTPUT_PIXELS = 3840 * 2160;
export const MAX_PRESENCES = 256;
const finite = values => Array.isArray(values) && values.every(Number.isFinite);
const same = (a,b) => JSON.stringify(a) === JSON.stringify(b);
const makeSurface = (w,h) => { const c = document.createElement('canvas'); c.width=w; c.height=h; return c; };
function dimensions(w,h,limit) {
  if (!Number.isSafeInteger(w)||!Number.isSafeInteger(h)||w<1||h<1||w>4096||h>4096||w*h>limit) throw Error('unsupported capture dimensions');
}
function rectangle(r) {
  if (!finite(r)||r.length!==4||r[2]<=0||r[3]<=0) throw Error('invalid pane rectangle');
  return [...r];
}
function intersects(a,b) { return a[0]<b[0]+b[2]&&b[0]<a[0]+a[2]&&a[1]<b[1]+b[3]&&b[1]<a[1]+a[3]; }

/** Host adapter. Call present synchronously from the mount's presentation
 * notification; expose only handle on the public mount. The layer must be
 * the mount's authenticated room live layer, never a caller-supplied roster.
 */
export function createPresentedSource({audience,app,canvas,layer,valid,now=()=>performance.now(),makeCanvas=makeSurface}) {
  sourceCanvases.add(canvas);
  const handle=Object.freeze({});
  let users=0,closed=false,copy,packet,error,observedLayer,stopMembers,lastGeneration=-1,lastFrame=-1,lastGeometry;
  const epochs=new Map();
  function release() {
    copy && (copy.width=copy.height=0);copy=undefined;packet=undefined;error=undefined;
    stopMembers?.();stopMembers=undefined;observedLayer=undefined;epochs.clear();
  }
  function watch(current) {
    if (current===observedLayer) return;
    stopMembers?.();epochs.clear();observedLayer=current;
    const members=current?.members()??[];
    if(members.length>MAX_PRESENCES)throw Error('presence capacity exceeded');
    for (const id of members) epochs.set(id,Symbol());
    stopMembers=current?.onMember(({publisher,active})=>{
      if(!active)epochs.delete(publisher);
      else if(!epochs.has(publisher)){
        if(epochs.size>=MAX_PRESENCES){error=Error('presence capacity exceeded');packet=undefined;}
        else epochs.set(publisher,Symbol());
      }
    });
  }
  const source={audience,
    acquire(){ if(closed)throw Error('source unavailable');users++;return()=>{if(--users===0)release();}; },
    read(){
      if(closed||!valid())throw Error('source unavailable');
      if(error)throw error;
      if(!packet)throw Error('source has no observed presented frame');
      if(observedLayer!==layer())throw Error('source audience unavailable');
      if(canvas.width!==packet.pixels[0]||canvas.height!==packet.pixels[1])throw Error('source resized before presentation');
      return packet;
    },
    member(p){return epochs.get(p.publisher)===p.epoch;},
  };
  sources.set(handle,source);
  return {handle,
    present(state){
      if(!users||closed)return;
      try{
        if(!valid()||state.phase!=='presented')throw Error('source unavailable before presentation');
        if(state.captureProtected!==false)throw Error('source capture protection is active or unknown');
        if(!app||app==='desktop'||typeof state.record!=='string'||!state.record)throw Error('capture requires an exact single-app record');
        dimensions(canvas.width,canvas.height,MAX_SOURCE_PIXELS);
        const size=state.logicalSize;
        if(!finite(size)||size.length!==2||size.some(v=>v<=0)||!same(state.surfaceSize,[canvas.width,canvas.height]))throw Error('incoherent source geometry');
        if(!Number.isSafeInteger(state.frames)||state.frames<1||!Number.isSafeInteger(state.gpuGeneration))throw Error('missing presentation generation');
        const geometry=[state.record,size,state.surfaceSize,state.layoutGeneration];
        if(state.gpuGeneration<lastGeneration||(state.gpuGeneration===lastGeneration&&state.frames<lastFrame))throw Error('out-of-order source presentation');
        if(state.gpuGeneration===lastGeneration&&state.frames===lastFrame){
          if(!same(lastGeometry,geometry))throw Error('geometry changed without a new presentation');
          return;
        }
        const current=layer();watch(current);
        const at=state.observedPresentedAtMs??now();
        if((current?.members().length??0)>MAX_PRESENCES)throw Error('presence capacity exceeded');
        const pointers=(current?.remoteState(at)??[]).filter(p=>epochs.has(p.publisher)&&!p.stale&&p.focus?.app===app&&p.focus?.record===state.record&&finite(p.pointer)&&p.pointer.length===2&&p.pointer[0]>=0&&p.pointer[1]>=0&&p.pointer[0]<size[0]&&p.pointer[1]<size[1]).map(p=>({publisher:p.publisher,seq:p.seq,receivedAtMs:p.receivedAtMs,pointer:[...p.pointer],participant:structuredClone(p.participant),epoch:epochs.get(p.publisher)}));
        if(pointers.length>MAX_PRESENCES)throw Error('presence capacity exceeded');
        copy??=makeCanvas(canvas.width,canvas.height);
        if(copy.width!==canvas.width)copy.width=canvas.width;
        if(copy.height!==canvas.height)copy.height=canvas.height;
        const ctx=copy.getContext('2d');if(!ctx)throw Error('Canvas2D capture unavailable');
        ctx.clearRect(0,0,copy.width,copy.height);ctx.drawImage(canvas,0,0);
        packet={image:copy,app,record:state.record,frame:state.frames,gpuGeneration:state.gpuGeneration,layoutGeneration:state.layoutGeneration,at,logical:[...size],pixels:[canvas.width,canvas.height],presenceAvailable:Boolean(current),pointers};
        lastGeneration=state.gpuGeneration;lastFrame=state.frames;lastGeometry=structuredClone(geometry);error=undefined;
      }catch(e){packet=undefined;error=e;}
    },
    close(){if(closed)return;closed=true;release();},
  };
}

/** sources: [{source: mount.captureSource, rect: [worldX,worldY,width,height]}].
 * Panes may not overlap and must share an authenticated observation audience.
 * Call render/png explicitly; visibility changes never drive agent execution.
 */
export function createPanorama({canvas,sources:inputs,width=1920,height=1080,now=()=>performance.now()}) {
  if(sourceCanvases.has(canvas))throw Error('a source canvas cannot be the capture output');
  dimensions(width,height,MAX_OUTPUT_PIXELS);
  if(!Array.isArray(inputs)||!inputs.length||inputs.length>MAX_PANES)throw Error('unsupported pane count');
  const rows=inputs.map(({source,rect},index)=>{
    const bound=sources.get(source);if(!bound)throw Error('unknown capture source');
    return {source:bound,rect:rectangle(rect),index,stop:null,wanted:true};
  });
  if(rows.some(r=>r.source.audience!==rows[0].source.audience))throw Error('capture audience mismatch');
  for(let i=0;i<rows.length;i++)for(let j=0;j<i;j++)if(intersects(rows[i].rect,rows[j].rect))throw Error('overlapping panes');
  const ctx=canvas.getContext('2d');if(!ctx)throw Error('Canvas2D capture unavailable');
  canvas.width=width;canvas.height=height;
  let visible=true,closed=false,encoding=false,sequence=0,lastAt=-Infinity;
  function subscribe(row,on){if(on&&!row.stop)row.stop=row.source.acquire();else if(!on&&row.stop){row.stop();row.stop=null;}}
  try{for(const row of rows)subscribe(row,true);}catch(error){for(const row of rows)subscribe(row,false);throw error;}
  function renderFrame(camera,{maxAgeMs=1000,maxSkewMs=1000}={}){
    if(closed)throw Error('observer closed');
    if(canvas.width!==width||canvas.height!==height){
      canvas.width=width;canvas.height=height;
      throw Error('output dimensions changed during capture');
    }
    if(!visible)return null;
    const {cx,cy,scale}=camera??{};
    if(![cx,cy,scale,maxAgeMs,maxSkewMs].every(Number.isFinite)||scale<0.05||scale>8||maxAgeMs<0||maxSkewMs<0)throw Error('invalid camera or freshness bounds');
    const at=now();if(!Number.isFinite(at)||at<lastAt)throw Error('observer clock regressed');
    const shown=[], candidates=[];
    // Validate every visible source before touching the output. Offscreen
    // panes release their copy and do no sampling/copying until revealed.
    for(const row of rows){
      const [x,y,w,h]=row.rect;
      const screen=[width/2+scale*(x-cx),height/2+scale*(y-cy),w*scale,h*scale];
      const show=intersects(screen,[0,0,width,height]);row.wanted=show;
      if(show)candidates.push({row,screen});
    }
    // A newly revealed source may already have closed. Finish *all* culling
    // before any acquisition (which can throw), just as before source reads.
    for(const row of rows)if(!row.wanted)subscribe(row,false);
    for(const {row} of candidates)subscribe(row,true);
    for(const {row,screen} of candidates){
      const frame=row.source.read();
      if(at<frame.at||at-frame.at>maxAgeMs)throw Error('stale source presentation');
      shown.push({row,frame,screen});
    }
    if(shown.length&&Math.max(...shown.map(x=>x.frame.at))-Math.min(...shown.map(x=>x.frame.at))>maxSkewMs)throw Error('source presentation skew');
    ctx.setTransform(1,0,0,1,0,0);ctx.clearRect(0,0,width,height);
    const panes=[];
    try{
      for(const {row,frame,screen} of shown){
        const [x,y,w,h]=screen,[lw,lh]=frame.logical;
        const transform=[w/lw,0,0,h/lh,x,y];
        ctx.save();
        try{
          ctx.beginPath();ctx.rect(Math.max(0,x),Math.max(0,y),Math.min(width,x+w)-Math.max(0,x),Math.min(height,y+h)-Math.max(0,y));ctx.clip();
          ctx.setTransform(...transform);ctx.drawImage(frame.image,0,0,lw,lh);
          const pointers=[];
          for(const p of frame.pointers){
            if(!row.source.member(p)||!Number.isFinite(p.receivedAtMs)||at-p.receivedAtMs>STALE_AFTER_MS)continue;
            const point=[x+p.pointer[0]*w/lw,y+p.pointer[1]*h/lh];
            if(point[0]<0||point[1]<0||point[0]>=width||point[1]>=height)continue;
            // Native-size markers remain readable under camera zoom. Their
            // whole label is clipped to this pane, never over its neighbour.
            ctx.setTransform(1,0,0,1,0,0);
            ctx.fillStyle=/^#[0-9a-f]{6}$/i.test(p.participant?.color)?p.participant.color:colorOf(p.publisher);
            ctx.beginPath();ctx.moveTo(point[0],point[1]);ctx.lineTo(point[0]+9,point[1]+14);ctx.lineTo(point[0]+2,point[1]+12);ctx.closePath();ctx.fill();
            ctx.font='11px system-ui';ctx.fillText(markerText(p.publisher,p.participant).slice(0,120),point[0]+12,point[1]+13,240);
            pointers.push({publisher:p.publisher,sequence:p.seq,receivedAtMs:p.receivedAtMs,screen:point,kind:'observed-pointer'});
          }
          panes.push({index:row.index,app:frame.app,record:frame.record,frame:frame.frame,gpuGeneration:frame.gpuGeneration,layoutGeneration:frame.layoutGeneration,presentedAtMs:frame.at,logicalSize:[...frame.logical],pixels:[...frame.pixels],presenceAvailable:frame.presenceAvailable,transform,pointers});
        }finally{ctx.restore();}
      }
    }catch(error){ctx.setTransform(1,0,0,1,0,0);ctx.clearRect(0,0,width,height);throw error;}
    lastAt=at;
    return {version:1,provenance:'shared-host composition of retained mounts',sequence:++sequence,observedAtMs:at,camera:{cx,cy,scale},size:[width,height],panes};
  }
  function render(camera,options){
    try{return renderFrame(camera,options);}catch(error){ctx.setTransform(1,0,0,1,0,0);ctx.clearRect(0,0,width,height);throw error;}
  }
  return {render,
    async png(camera,options){
      if(encoding)throw Error('PNG encoding already in flight');
      encoding=true;
      try{
        const receipt=render(camera,options);if(!receipt)throw Error('observer hidden');
        const blob=await new Promise((resolve,reject)=>canvas.toBlob(value=>value?resolve(value):reject(Error('PNG encoding failed')),'image/png'));
        if(closed)throw Error('observer closed during capture');
        return {blob,receipt};
      }finally{encoding=false;}
    },
    setVisible(value){if(closed)throw Error('observer closed');visible=Boolean(value);for(const row of rows)subscribe(row,visible&&row.wanted);if(!visible){ctx.clearRect(0,0,width,height);}},
    close(){if(closed)return;closed=true;for(const row of rows)subscribe(row,false);ctx.clearRect(0,0,width,height);},
  };
}
