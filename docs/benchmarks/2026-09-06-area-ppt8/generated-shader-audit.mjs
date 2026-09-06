// Read-only generated WGSL source audit. Not a compiled-binary equivalence proof.
import fs from "node:fs";
import crypto from "node:crypto";
import {fileURLToPath} from "node:url";
const names=["fine_area","fine_msaa8","fine_msaa16","fine_sparse_area","fine_sparse_area_quad_skip","fine_sparse_area_segment_row_mask","fine_sparse_area_solid_fuse"];
const hash=s=>crypto.createHash("sha256").update(s).digest("hex");
function withoutComments(source){
 let out="",i=0;
 while(i<source.length){
  if(source.slice(i,i+2)==="//"){while(i<source.length&&source[i]!=="\n")i++;out+=" ";continue;}
  if(source.slice(i,i+2)==="/*"){i+=2;let depth=1;while(i<source.length&&depth){if(source.slice(i,i+2)==="/*"){depth++;i+=2;}else if(source.slice(i,i+2)==="*/"){depth--;i+=2;}else i++;}if(depth)throw Error("unterminated comment");out+=" ";continue;}
  out+=source[i++];
 }
 return out;
}
export function generatedHashes(path){
 const text=fs.readFileSync(path,"utf8"),blocks=[...text.matchAll(/^        (\w+): ComputeShader \{\n([\s\S]*?)^        \},$/gm)];
 const result={path,mtime_ms:fs.statSync(path).mtimeMs,has_ppt8:blocks.some(m=>m[1]==="fine_area_ppt8"),shaders:{}};
 for(const name of names){
  const block=blocks.find(m=>m[1]===name);if(!block)throw Error("missing shader "+name);
  const match=/code: Cow::Borrowed\(("(?:\\.|[^"\\])*")\),/.exec(block[2]);if(!match)throw Error("no WGSL for "+name);
  const raw=JSON.parse(match[1]);
  // Keep token boundaries: whitespace/comment removal cannot merge identifiers.
  const tokens=withoutComments(raw).match(/[A-Za-z_][A-Za-z_0-9]*|(?:\d+\.\d*|\.\d+|\d+)(?:[eE][+-]?\d+)?[fhiu]?|>>=|<<=|==|!=|<=|>=|&&|\|\||->|<<|>>|\+=|-=|\*=|\/=|%=|&=|\|=|\^=|\+\+|--|[^\s]/g)??[];
  const metadata=["workgroup_size","bindings","workgroup_buffers","binding_indices"].map(k=>block[2].split("\n").find(l=>l.trimStart().startsWith(k))?.trim()??null);
  if(metadata.some(v=>v===null))throw Error("missing metadata");
  result.shaders[name]={raw_source_sha256:hash(raw),normalized_tokens_sha256:hash(tokens.join(" ")),metadata_sha256:hash(JSON.stringify(metadata))};
 }
 return result;
}
export function inventory(){
 const paths=[];
 for(const mode of ["debug","release"]){
  const root="target/"+mode+"/build";
  for(const entry of fs.readdirSync(root))if(entry.startsWith("vello_shaders-")){
   const path=root+"/"+entry+"/out/shaders.rs";if(fs.existsSync(path))paths.push(path);
  }
 }
 return paths.map(path=>{try{return generatedHashes(path);}catch(error){return{path,error:String(error)};}}).sort((a,b)=>(b.mtime_ms??0)-(a.mtime_ms??0));
}
if(process.argv[1]===fileURLToPath(import.meta.url))console.log(JSON.stringify(inventory(),null,2));
