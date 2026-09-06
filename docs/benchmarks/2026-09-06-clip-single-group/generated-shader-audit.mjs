// Read-only generated-source and metadata audit, not compiled-binary equivalence.
import fs from "node:fs";
import crypto from "node:crypto";
import {fileURLToPath} from "node:url";
const hash=s=>crypto.createHash("sha256").update(s).digest("hex");
export function withoutComments(source){
 let out="",i=0;
 while(i<source.length){
  if(source.slice(i,i+2)==="//"){while(i<source.length&&source[i]!=="\n")i++;out+=" ";continue;}
  if(source.slice(i,i+2)==="/*"){i+=2;let depth=1;while(i<source.length&&depth){if(source.slice(i,i+2)==="/*"){depth++;i+=2;}else if(source.slice(i,i+2)==="*/"){depth--;i+=2;}else i++;}if(depth)throw Error("unterminated comment");out+=" ";continue;}
  out+=source[i++];
 }
 return out;
}
export function normalized(source){return (withoutComments(source).match(/[A-Za-z_][A-Za-z_0-9]*|(?:\d+\.\d*|\.\d+|\d+)(?:[eE][+-]?\d+)?[fhiu]?|>>=|<<=|==|!=|<=|>=|&&|\|\||->|<<|>>|\+=|-=|\*=|\/=|%=|&=|\|=|\^=|\+\+|--|[^\s]/g)??[]).join(" ");}
export function shaders(path){
 const text=fs.readFileSync(path,"utf8"),blocks=[...text.matchAll(/^        (\w+): ComputeShader \{\n([\s\S]*?)^        \},$/gm)];
 if(!blocks.length)throw Error("no generated shaders");
 return Object.fromEntries(blocks.map(m=>{
  const c=/code: Cow::Borrowed\(("(?:\\.|[^"\\])*")\),/.exec(m[2]);if(!c)throw Error("missing code: "+m[1]);
  const raw=JSON.parse(c[1]);
  const metadata=Object.fromEntries(["workgroup_size","bindings","workgroup_buffers","binding_indices"].map(k=>{
   const line=m[2].split("\n").find(l=>new RegExp("^"+k+"\\s*:").test(l.trimStart()));if(!line)throw Error("missing "+k);
   return [k,line.trim()];
  }));
  return[m[1],{raw,metadata}];
 }));
}
export function generatedHashes(path){
 const entries=shaders(path);
 return{path,mtime_ms:fs.statSync(path).mtimeMs,has_ppt8:!!entries.fine_area_ppt8,has_clip_single:!!entries.clip_leaf_single_group,
 shaders:Object.fromEntries(Object.entries(entries).map(([name,{raw,metadata}])=>[name,{raw_source_sha256:hash(raw),normalized_tokens_sha256:hash(normalized(raw)),metadata_sha256:hash(JSON.stringify(metadata))}]))};
}
export function inventory(){
 const paths=[];
 for(const mode of ["debug","release"]){
  const root="target/"+mode+"/build";
  for(const e of fs.readdirSync(root))if(e.startsWith("vello_shaders-")){const p=root+"/"+e+"/out/shaders.rs";if(fs.existsSync(p))paths.push(p);}
 }
 return paths.map(generatedHashes).sort((a,b)=>b.mtime_ms-a.mtime_ms);
}
if(process.argv[1]===fileURLToPath(import.meta.url))console.log(JSON.stringify(inventory(),null,2));
