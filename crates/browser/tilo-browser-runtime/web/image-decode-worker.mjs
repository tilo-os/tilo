// Decodes the page's images off its main thread (web/image-decode.mjs):
// each message names its request, and the answer carries the RGBA it read
// back, handed over without a copy, or why it failed.
import {decodeImage} from './image-decode.mjs';

self.onmessage=async({data:{id,bytes,maxDimension}})=>{
  try{
    const image=await decodeImage(bytes,maxDimension);
    self.postMessage({id,image},[image.rgba.buffer]);
  }catch(error){
    self.postMessage({id,error:String(error?.message??error)});
  }
};
