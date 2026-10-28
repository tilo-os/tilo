// Explicit synthetic-only pixel qualification; no mounted apps or desktop capture.
// Requires the existing Chrome selected by tools/web/chrome.mjs. Installs nothing.
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {serve,launch} from '../../../../tools/web/chrome.mjs';
const server=await serve(fileURLToPath(new URL('../web/',import.meta.url)));
let browser;
try{
 browser=await launch();const page=await browser.tab();await page.navigate(server.url+'/panorama.mjs');
 const result=await page.evaluate(`(async()=>{
  const {createPresentedSource,createPanorama}=await import('/panorama.mjs');
  const input=document.createElement('canvas');input.width=200;input.height=100;
  const c=input.getContext('2d');c.fillStyle='#ff0000';c.fillRect(0,0,200,100);c.fillStyle='#0000ff';c.fillRect(40,20,20,20);
  let t=0;const layer={members:()=>['fixture-agent'],onMember:()=>()=>{},remoteState:()=>[{publisher:'fixture-agent',seq:1,receivedAtMs:0,focus:{app:'fixture',record:'synthetic-only'},pointer:{window:{app:'fixture',record:'synthetic-only'},at:[20,10]},participant:{label:'Test only',color:'#00ff00'}}]};
  const source=createPresentedSource({audience:'synthetic-only',app:'fixture',canvas:input,layer:()=>layer,valid:()=>true,now:()=>t});
  const output=document.createElement('canvas');
  const view=createPanorama({canvas:output,sources:[{source:source.handle,rect:[100,50,100,50]}],width:200,height:100,now:()=>t});
  source.present({phase:'presented',captureProtected:false,frames:1,gpuGeneration:1,record:'synthetic-only',logicalSize:[100,50],surfaceSize:[200,100]});
  // Changing the live source after presentation must not change the copied frame.
  c.fillStyle='#ffffff';c.fillRect(0,0,200,100);
  const capture=await view.png({cx:150,cy:75,scale:1});
  const pixels=output.getContext('2d');const pixel=(x,y)=>[...pixels.getImageData(x,y,1,1).data];
  const checks={outside:pixel(10,10),pane:pixel(51,26),pointer:pixel(74,44),content:pixel(79,36)};
  const bitmap=await createImageBitmap(capture.blob);const png=document.createElement('canvas');png.width=200;png.height=100;png.getContext('2d').drawImage(bitmap,0,0);checks.pngPane=[...png.getContext('2d').getImageData(51,26,1,1).data];bitmap.close();
  const receipt=capture.receipt;view.close();source.close();
  // Exercise the browser's real WebGPU-to-Canvas2D handoff as used by host.mjs.
  const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU adapter unavailable');
  const device=await adapter.requestDevice();const gpu=document.createElement('canvas');gpu.width=64;gpu.height=32;
  const context=gpu.getContext('webgpu');context.configure({device,format:navigator.gpu.getPreferredCanvasFormat(),alphaMode:'opaque'});
  const encoder=device.createCommandEncoder();const pass=encoder.beginRenderPass({colorAttachments:[{view:context.getCurrentTexture().createView(),clearValue:{r:0,g:0,b:1,a:1},loadOp:'clear',storeOp:'store'}]});pass.end();device.queue.submit([encoder.finish()]);
  const s=createPresentedSource({audience:'synthetic-only',app:'fixture',canvas:gpu,layer:()=>undefined,valid:()=>true});
  const out=document.createElement('canvas');const v=createPanorama({canvas:out,sources:[{source:s.handle,rect:[0,0,64,32]}],width:64,height:32});
  await device.queue.onSubmittedWorkDone();
  s.present({phase:'presented',captureProtected:false,frames:1,gpuGeneration:1,record:'gpu-fixture',logicalSize:[64,32],surfaceSize:[64,32]});
  v.render({cx:32,cy:16,scale:1});checks.webgpu=[...out.getContext('2d').getImageData(20,10,1,1).data];
  v.close();s.close();context.unconfigure();device.destroy();
  return {checks,receipt,pngBytes:capture.blob.size};
 })()`);
 assert.deepEqual(result.checks.outside,[0,0,0,0]);assert.deepEqual(result.checks.pane,[255,0,0,255]);
 assert.deepEqual(result.checks.pngPane,[255,0,0,255]);assert.deepEqual(result.checks.pointer,[0,255,0,255]);
 assert.deepEqual(result.checks.content,[0,0,255,255]);assert.deepEqual(result.checks.webgpu,[0,0,255,255]);
 assert.equal(result.receipt.panes[0].pointers.length,1);assert.ok(result.pngBytes>100);
 console.log(JSON.stringify({status:'PASS',scope:'synthetic Canvas2D/WebGPU source + PNG pixels only; no agents, app content or performance measurement',...result},null,2));
}finally{await browser?.close();await server.close();}
