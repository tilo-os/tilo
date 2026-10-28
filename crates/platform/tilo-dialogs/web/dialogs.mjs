// The browser backend of tilo-dialogs (src/web.rs): file choosers, saving
// and URI opening. Rust calls them while the app handles an input event,
// usually inside that gesture's user activation. Browsers refuse pickers and
// pop-ups without one, silently, so a chooser asked for after the gesture
// expired asks the person once more on the page instead of doing nothing, and
// a save without one downloads. The last parameter of each is the
// environment, replaced by tests.

/** Everything chosen in one pick is read into memory; refuse beyond this. */
export const MAX_PICK_BYTES=256*1024*1024;

/** Whether the page has no user activation right now, so the browser would
 * ignore a picker. Unknown (no User Activation API) counts as active. */
function gestureExpired(nav){
  const activation=nav?.userActivation;
  return activation!=null&&activation.isActive===false;
}

/** Ask on the page for the click a picker needs. Resolves true for go
 * ahead, false for cancel. */
function askForGesture(doc,title,acceptLabel){
  return new Promise(resolve=>{
    const card=doc.createElement('div');
    card.setAttribute?.('role','dialog');
    card.setAttribute?.('aria-modal','true');
    card.setAttribute?.('aria-label',title);
    Object.assign(card.style,{position:'fixed',left:'50%',top:'50%',transform:'translate(-50%,-50%)',zIndex:'2147483647',
      padding:'16px',borderRadius:'12px',background:'#1d1f24',color:'#eef0f4',font:'14px system-ui,sans-serif',
      boxShadow:'0 12px 40px rgba(0,0,0,.45)',display:'flex',flexDirection:'column',gap:'12px',minWidth:'260px'});
    const heading=doc.createElement('p');
    heading.textContent=title;
    Object.assign(heading.style,{margin:'0',fontWeight:'600'});
    const row=doc.createElement('div');
    Object.assign(row.style,{display:'flex',gap:'8px',justifyContent:'flex-end'});
    const button=(label,primary,answer)=>{
      const node=doc.createElement('button');
      node.type='button';node.textContent=label;
      Object.assign(node.style,{padding:'6px 14px',borderRadius:'8px',border:'0',font:'inherit',cursor:'pointer',
        background:primary?'#4c6ef5':'#30333a',color:'#fff'});
      node.addEventListener('click',()=>{card.remove();resolve(answer);},{once:true});
      return node;
    };
    const go=button(acceptLabel||'Choose…',true,true);
    row.append(button('Cancel',false,false),go);
    card.append(heading,row);
    doc.body.append(card);
    go.focus?.();
  });
}

/** Open an `<input type=file>` chooser. Resolves `[{name,type,bytes}]`, or
 * `null` when the user cancels. `accept` is the attribute value ('' for any).
 * `title` and `acceptLabel` name the chooser when the page must ask for a
 * click first. */
export function pickFiles(accept,multiple,maxContentsBytes=null,title='Choose a file',acceptLabel='Choose…',{document:doc=globalThis.document,navigator:nav=globalThis.navigator}={}){
  return new Promise((resolve,reject)=>{
    if(maxContentsBytes!=null&&(!Number.isSafeInteger(maxContentsBytes)||maxContentsBytes<0)){
      reject(Error('Invalid contents byte limit'));return;
    }
    const limit=Math.min(MAX_PICK_BYTES,maxContentsBytes??MAX_PICK_BYTES);
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
        if(total>limit)throw Error(`Chosen files are too large (${total} bytes; limit ${limit})`);
        const picked=[];
        let remaining=limit;
        for(const file of files){
          const bytes=new Uint8Array(await file.slice(0,remaining+1).arrayBuffer());
          if(bytes.byteLength>remaining)throw Error(`Chosen contents are too large (limit ${limit})`);
          remaining-=bytes.byteLength;
          picked.push({name:file.name,type:file.type||null,bytes});
        }
        settle(resolve,picked);
      }catch(error){settle(reject,error);}
    },{once:true});
    doc.body.append(input);
    const open=()=>{try{input.click();}catch(error){settle(reject,error);}};
    if(!gestureExpired(nav)){open();return;}
    askForGesture(doc,title,acceptLabel).then(go=>go?open():settle(resolve,null));
  });
}

/** Download `bytes` as `name`. The browser, not the page, picks the folder. */
export function downloadFile(name,bytes,{document:doc=globalThis.document,URL:urls=globalThis.URL,Blob:BlobType=globalThis.Blob,setTimeout:later=globalThis.setTimeout}={}){
  const href=urls.createObjectURL(new BlobType([bytes],{type:'application/octet-stream'}));
  const link=doc.createElement('a');
  link.href=href;link.download=name||'download';link.rel='noopener';link.style.display='none';
  doc.body.append(link);
  try{link.click();}finally{link.remove();later(()=>urls.revokeObjectURL(href),60_000);}
}

/** Where a save will go, chosen now, written later: the browser's own Save
 * dialog where it has one (File System Access) and the gesture still allows
 * it, else a download under `name`. Resolves `{handle}` or `{download}`, or
 * `null` when the person cancelled the Save dialog. The picker is asked
 * before the first await, inside the caller's gesture. */
export async function chooseSaveTarget(name,{window:win=globalThis}={}){
  const picker=win?.showSaveFilePicker;
  if(typeof picker==='function'&&!gestureExpired(win?.navigator)){
    try{
      return {handle:await picker.call(win,{suggestedName:name||undefined}),name};
    }catch(error){
      if(error?.name==='AbortError')return null;
      // No activation after all, or a name the picker refuses: download.
    }
  }
  return {download:name||'download',name};
}

/** Write `bytes` to a target from `chooseSaveTarget`. */
export async function writeSaveTarget(target,bytes,env={}){
  if(target?.handle){
    const writable=await target.handle.createWritable();
    try{await writable.write(bytes);await writable.close();}
    catch(error){try{await writable.abort?.();}catch{}throw error;}
    return;
  }
  downloadFile(target?.download??'download',bytes,env);
}

/** Save `bytes` where the person chooses (see `chooseSaveTarget`). Resolves
 * true once written, false when they cancelled. */
export async function saveFile(name,bytes,env={}){
  const target=await chooseSaveTarget(name,env);
  if(target===null)return false;
  await writeSaveTarget(target,bytes,env);
  return true;
}

/** Open a URI (already vetted by Rust) in a new browsing context without
 * giving it a handle on this page. `false` when a pop-up blocker refused. */
export function openUri(uri,{window:win=globalThis.window}={}){
  const opened=win?.open(uri,'_blank');
  if(!opened)return false;
  try{opened.opener=null;}catch{}
  return true;
}
