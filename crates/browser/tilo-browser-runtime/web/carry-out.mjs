// The workspace's files carried out of the page onto the computer
// (src/browser/carry_out.rs): saved where the person chooses, or dragged onto
// its desktop or file manager. The bytes come from a link the workspace's
// host serves them at; the page never holds a file whole.
//
// Saving: Chrome and Edge open their Save dialog (File System Access) and the
// page pipes the link's response into the file, a buffer at a time. Firefox
// and Safari have no such dialog; the browser downloads the link itself, which
// its download manager streams to disk.
//
// Dragging out: Chrome and Edge on Windows and macOS save a drag carrying a
// `DownloadURL` (`mime:name:url`) where it is dropped, fetching the URL with
// their own download manager; it never reaches the page or its service worker,
// so the URL is the host's. A drag starts only from a real element under the
// pointer, so the control's element in the semantic mirror is made draggable
// (`draggable`, `data-tilo-carries`), and the pointer events it now takes are
// handed on to the canvas as they came. While such a drag stays over the page
// it is still the window's own drag: the moves it makes go on to the canvas,
// and a drop in the page lets go there. Only a drag that leaves the page and
// lands on the computer saves the file. Firefox and Safari, and Chrome on
// Linux, take no `DownloadURL` drag: there the drop outside does nothing.

/** Page milliseconds, for the link's expiry. */
export function nowMs(){return globalThis.performance?.now?.()??Date.now();}

/** Whether the page has no user activation right now. Unknown counts as active. */
function gestureExpired(nav){
  const activation=nav?.userActivation;
  return activation!=null&&activation.isActive===false;
}

/** Where a save goes, chosen now: the browser's Save dialog where it has one
 * and the gesture still allows it (`{handle}`), else a download (`{download}`).
 * `null` when the person cancelled the dialog. The dialog is asked before the
 * first await, inside the caller's gesture. */
export async function chooseCarryTarget(name,{window:win=globalThis}={}){
  const picker=win?.showSaveFilePicker;
  if(typeof picker==='function'&&!gestureExpired(win?.navigator)){
    try{
      return {handle:await picker.call(win,{suggestedName:name||undefined})};
    }catch(error){
      if(error?.name==='AbortError')return null;
      // No activation after all, or a name the dialog refuses: download.
    }
  }
  return {download:name||'download'};
}

/** How much of a response is read before it is written: one buffer, used
 * again for every piece. */
export const COPY_BUFFER_BYTES=1<<20;

/** Copy `body` into `writable`, reading into one buffer used again for every
 * piece, so a file of any size takes the page one buffer. A stream that
 * hands each piece over in a fresh buffer instead (`pipeTo`, or a reader
 * without `byob`) leaves the page's collector a gigabyte of them to find
 * for a gigabyte saved, faster than it collects: a 1 GB save grew the
 * page by 1.4 GB that way. */
export async function copyInto(body,writable){
  let reader;
  try{reader=body.getReader({mode:'byob'});}
  catch{return body.pipeTo(writable);}
  const writer=writable.getWriter();
  let buffer=new ArrayBuffer(COPY_BUFFER_BYTES);
  try{
    for(;;){
      const {value,done}=await reader.read(new Uint8Array(buffer));
      if(done)break;
      // The write has taken the piece once it resolves: the buffer is free.
      await writer.write(value);
      buffer=value.buffer;
    }
    await writer.close();
  }catch(error){
    try{await reader.cancel(error);}catch{}
    try{await writer.abort(error);}catch{}
    throw error;
  }
}

/** Save what `url` serves to `target` (from `chooseCarryTarget`): streamed into
 * the chosen file, or downloaded by the browser. */
export async function saveFromLink(target,url,{fetch:fetcher=globalThis.fetch,document:doc=globalThis.document}={}){
  if(target?.handle){
    const response=await fetcher(url,{credentials:'omit',cache:'no-store'});
    if(!response.ok)throw new Error((await response.text().catch(()=>''))||`the host answered ${response.status}`);
    const writable=await target.handle.createWritable();
    try{await copyInto(response.body,writable);}
    catch(error){try{await writable.abort?.();}catch{}throw error;}
    return;
  }
  // The host's answer says to save it (`Content-Disposition: attachment`), so
  // following the link downloads it and leaves the page where it is.
  const link=doc.createElement('a');
  link.href=url;link.download=target?.download??'';link.rel='noopener noreferrer';link.style.display='none';
  doc.body.append(link);
  try{link.click();}finally{link.remove();}
}

/** A tab of the browser's own for a file shown from its link, opened now,
 * inside the person's click, which a browser asks a new tab for; pointed at
 * the link once it is minted (`showInTab`). `null` where the browser refused
 * the tab. */
export function openShowTab({window:win=globalThis}={}){
  const tab=win.open?.('about:blank','_blank');
  if(tab)tab.opener=null;
  return tab??null;
}

/** Show what `url` serves in `tab` (from `openShowTab`), or close the tab
 * when there is nothing to show (`url` empty). */
export function showInTab(tab,url){
  if(!url){tab?.close?.();return;}
  tab.location.replace(`${url}?view`);
}

// What the canvas's pointer listeners (winit's) take. Crossing between the
// canvas and a carrying element (pointerover, pointerout and their enter and
// leave) is no entry or exit of the surface: only one from or to outside it
// is handed on.
const FORWARDED=['pointerdown','pointermove','pointerup','pointercancel','contextmenu'];
const CROSSING=['pointerover','pointerout','pointerenter','pointerleave'];

/** Make the semantic mirror's carrying elements under `root` drag out of the
 * page, their pointer handed on to `canvas`. `link(key)` answers
 * `{url, mime, name}` for a control's key once its link is here, and asks for
 * it otherwise: it is asked as the pointer comes near a control, and as a
 * drag starts. */
export function createCarryOut({root,canvas,link,view=globalThis}){
  const doc=view.document;
  const listeners=new AbortController();
  const on=(node,type,fn,options={})=>node.addEventListener(type,fn,{...options,signal:listeners.signal});
  const carrying=target=>target?.closest?.('[data-tilo-carries]');
  // The drag out under way: its pointer, where it last was, and whether it
  // was dropped in the page.
  let drag=null;
  // The last pointer event a carrying element took, for the moves a drag makes.
  let last=null;
  function pointer(type,source,{clientX,clientY}=source){
    const Kind=source.pointerType!==undefined&&view.PointerEvent?view.PointerEvent:view.MouseEvent;
    const event=new Kind(type,{
      bubbles:false,cancelable:true,composed:false,
      clientX,clientY,screenX:source.screenX,screenY:source.screenY,
      button:type==='pointermove'?-1:(source.button??0),buttons:type==='pointerup'||type==='pointercancel'?0:(source.buttons??1),
      ctrlKey:source.ctrlKey,shiftKey:source.shiftKey,altKey:source.altKey,metaKey:source.metaKey,
      pointerId:source.pointerId??1,pointerType:source.pointerType??'mouse',isPrimary:source.isPrimary??true,
      pressure:type==='pointerup'?0:(source.pressure??0.5),
    });
    canvas.dispatchEvent(event);
    return event;
  }
  // Between the canvas and a carrying element the pointer never left the
  // surface: the canvas does not hear it leave, nor come back.
  const inside=node=>node===canvas||Boolean(carrying(node));
  for(const type of CROSSING){
    on(canvas,type,event=>{if(carrying(event.relatedTarget))event.stopImmediatePropagation();},{capture:true});
    on(root,type,event=>{
      if(!carrying(event.target)||inside(event.relatedTarget))return;
      if(type==='pointerover')link(carrying(event.target).dataset.tiloCarries);
      if(type==='pointerover'||type==='pointerout')pointer(type,event);
    },{capture:true});
  }
  // The press and release the canvas took are the click: the element's own
  // activation is for the keyboard and assistive technology (a click with no
  // pointer behind it, `detail` 0).
  on(root,'click',event=>{
    if(!carrying(event.target)||!event.detail)return;
    event.stopPropagation();event.preventDefault();
  },{capture:true});
  for(const type of FORWARDED){
    on(root,type,event=>{
      const element=carrying(event.target);
      if(!element)return;
      // The browser cancels the pointer as its drag starts; the drag goes on
      // as the window's own, fed from the drag's own events below.
      if(type==='pointercancel'&&drag)return;
      if(type==='pointerdown')link(element.dataset.tiloCarries);
      if(event.pointerType!==undefined)last=event;
      const copy=pointer(type,event);
      if(copy.defaultPrevented&&type==='contextmenu')event.preventDefault();
    },{capture:true});
  }
  // A wheel over a carrying element scrolls what is under it on the canvas.
  on(root,'wheel',event=>{
    if(!carrying(event.target))return;
    event.preventDefault();
    canvas.dispatchEvent(new view.WheelEvent('wheel',{
      bubbles:false,cancelable:true,clientX:event.clientX,clientY:event.clientY,screenX:event.screenX,screenY:event.screenY,
      deltaX:event.deltaX,deltaY:event.deltaY,deltaZ:event.deltaZ,deltaMode:event.deltaMode,
      ctrlKey:event.ctrlKey,shiftKey:event.shiftKey,altKey:event.altKey,metaKey:event.metaKey,buttons:event.buttons,
    }));
  },{capture:true,passive:false});
  // A press may start a drag (its default), but must not take the focus from
  // the canvas's editor as an element's press would: it goes back where it was.
  on(root,'mousedown',event=>{
    if(!carrying(event.target))return;
    const before=doc.activeElement;
    view.setTimeout(()=>{
      if(doc.activeElement!==before&&before&&typeof before.focus==='function')before.focus({preventScroll:true});
    },0);
  },{capture:true});
  on(root,'dragstart',event=>{
    const element=carrying(event.target);
    if(!element)return;
    const found=link(element.dataset.tiloCarries);
    const data=event.dataTransfer;
    if(!found||!data){
      // No link yet: no file drag, and the press stays the window's.
      event.preventDefault();
      return;
    }
    data.effectAllowed='copy';
    data.setData('DownloadURL',`${found.mime}:${found.name.replace(/:/g,'_')}:${found.url}`);
    drag={source:last??event,x:event.clientX,y:event.clientY,dropped:false};
  });
  // Over the page the drag is the window's: its moves go on to the canvas,
  // and the page takes the drop.
  on(doc,'dragover',event=>{
    if(!drag)return;
    event.preventDefault();
    if(event.dataTransfer)event.dataTransfer.dropEffect='copy';
    if(event.clientX===drag.x&&event.clientY===drag.y)return;
    drag.x=event.clientX;drag.y=event.clientY;
    pointer('pointermove',drag.source,event);
  },{capture:true});
  on(doc,'drop',event=>{
    if(!drag)return;
    event.preventDefault();event.stopImmediatePropagation();
    drag.dropped=true;
    pointer('pointerup',drag.source,event);
  },{capture:true});
  on(root,'dragend',()=>{
    if(!drag)return;
    // Dropped outside the page (the computer saved it there) or given up:
    // the window's drag ends without landing anywhere in the page.
    if(!drag.dropped)pointer('pointercancel',drag.source,{clientX:drag.x,clientY:drag.y});
    drag=null;
  },{capture:true});
  return {dragging:()=>drag!==null,close:()=>listeners.abort()};
}
