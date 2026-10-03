// The browser backend of tilo-dialogs (src/web.rs): file choosers, downloads
// and URI opening. Rust calls them while the app handles an input event,
// inside that gesture's user activation; browsers refuse pickers and pop-ups
// otherwise. The last parameter of each is the environment, replaced by tests.

/** Everything chosen in one pick is read into memory; refuse beyond this. */
export const MAX_PICK_BYTES=256*1024*1024;

/** Open an `<input type=file>` chooser. Resolves `[{name,type,bytes}]`, or
 * `null` when the user cancels. `accept` is the attribute value ('' for any). */
export function pickFiles(accept,multiple,{document:doc=globalThis.document}={}){
  return new Promise((resolve,reject)=>{
    const input=doc.createElement('input');
    input.type='file';input.multiple=Boolean(multiple);if(accept)input.accept=accept;input.style.display='none';
    let settled=false;
    const settle=(fn,value)=>{if(settled)return;settled=true;input.remove();fn(value);};
    input.addEventListener('cancel',()=>settle(resolve,null),{once:true});
    input.addEventListener('change',async()=>{
      try{
        const files=[...(input.files??[])];
        if(!files.length){settle(resolve,null);return;}
        const total=files.reduce((sum,file)=>sum+file.size,0);
        if(total>MAX_PICK_BYTES)throw Error(`Chosen files are too large (${total} bytes; limit ${MAX_PICK_BYTES})`);
        settle(resolve,await Promise.all(files.map(async file=>({name:file.name,type:file.type||null,bytes:new Uint8Array(await file.arrayBuffer())}))));
      }catch(error){settle(reject,error);}
    },{once:true});
    doc.body.append(input);
    try{input.click();}catch(error){settle(reject,error);}
  });
}

/** Download `bytes` as `name`. The browser, not the page, picks the folder. */
export function saveFile(name,bytes,{document:doc=globalThis.document,URL:urls=globalThis.URL,Blob:BlobType=globalThis.Blob,setTimeout:later=globalThis.setTimeout}={}){
  const href=urls.createObjectURL(new BlobType([bytes],{type:'application/octet-stream'}));
  const link=doc.createElement('a');
  link.href=href;link.download=name||'download';link.rel='noopener';link.style.display='none';
  doc.body.append(link);
  try{link.click();}finally{link.remove();later(()=>urls.revokeObjectURL(href),60_000);}
}

/** Open a URI (already vetted by Rust) in a new browsing context without
 * giving it a handle on this page. `false` when a pop-up blocker refused. */
export function openUri(uri,{window:win=globalThis.window}={}){
  const opened=win?.open(uri,'_blank');
  if(!opened)return false;
  try{opened.opener=null;}catch{}
  return true;
}
