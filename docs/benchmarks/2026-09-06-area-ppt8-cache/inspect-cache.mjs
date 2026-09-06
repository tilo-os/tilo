import {readdir,stat,readFile} from "node:fs/promises";
import {createHash} from "node:crypto";
import {resolve,join,relative} from "node:path";
const root=resolve(process.argv[2]);
if(!root.startsWith("/tmp/qos-ppt8-cache-20260906."))throw Error("unexpected cache root");
const entries=[];
async function walk(path){
 for(const e of (await readdir(path,{withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))){
  const p=join(path,e.name);
  if(e.isDirectory())await walk(p);
  else if(e.isFile()){const s=await stat(p);entries.push({path:relative(root,p),bytes:s.size,sha256:createHash("sha256").update(await readFile(p)).digest("hex")});}
  else throw Error("unexpected non-file cache entry");
 }
}
await walk(root);
console.log(JSON.stringify({root,files:entries.length,bytes:entries.reduce((s,e)=>s+e.bytes,0),entries},null,2));
