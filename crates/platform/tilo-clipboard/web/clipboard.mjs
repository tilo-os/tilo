// The clipboard backend (src/web.rs) needs a synchronous publication result
// (Cut must not delete text after a denied write). Reuse the browser's native
// copy event. Async Clipboard.writeText cannot satisfy that contract.
// `report(true)` or `report(false, reason)` hears the outcome; the reason is
// said to the person as it is, so it is one of these sentences, never the
// browser's own error text.

/** The browser turned the copy down: no user gesture it accepts, a setting,
 * or a transfer it would not take. */
export const COPY_REFUSED='The browser did not allow copying here. Try the keyboard Copy shortcut.';
/** The page has no way to reach the clipboard at all. */
export const COPY_UNAVAILABLE='This browser cannot copy from a page. Try the keyboard Copy shortcut.';
export function writeClipboardText(text, report, document=globalThis.document) {
  return writeClipboard({'text/plain':text}, report, document);
}

// Formatted text: `html` as text/html with `plain` as its text/plain fallback,
// through the same synchronous copy event.
export function writeClipboardRich(html, plain, report, document=globalThis.document) {
  return writeClipboard({'text/html':html,'text/plain':plain}, report, document);
}

function writeClipboard(data, report, document) {
  if(!document?.execCommand){
    report(false,COPY_UNAVAILABLE);
    return false;
  }
  let written=false, failure;
  const transfer=event=>{
    // Do not reenter the WASM keyboard handler while it is dispatching a menu.
    event.stopImmediatePropagation();
    event.preventDefault();
    try {
      if(!event.clipboardData)throw Error('the copy event carries no clipboard data');
      for(const [type,value] of Object.entries(data))event.clipboardData.setData(type,value);
      written=true;
    } catch(error) { failure=error; }
  };
  try {
    document.addEventListener('copy',transfer,{capture:true});
    const completed=document.execCommand('copy');
    if(!completed||!written||failure)throw failure??Error('the browser declined the copy command');
    report(true);
    return true;
  } catch(error) {
    // The cause is for whoever debugs the page; the person hears what to do.
    console.warn('clipboard: copy refused',error);
    report(false,COPY_REFUSED);
    return false;
  } finally {
    document.removeEventListener('copy',transfer,{capture:true});
  }
}

// A copy claimed inside the click for text that arrives after it (a link the
// click asks a host for): the async clipboard takes a promised item while
// the click still counts as one, and writes it once `fill(text)` resolves
// it. `fill(null)` gives the claim up without reporting a failure. Returns
// `fill`, or `null` where the browser cannot hold a copy open.
export function reserveClipboardText(report, navigator=globalThis.navigator, Item=globalThis.ClipboardItem) {
  if(typeof navigator?.clipboard?.write!=='function'||typeof Item!=='function')return null;
  let resolve, reject, given=false;
  const text=new Promise((ok,fail)=>{resolve=ok;reject=fail;});
  let written;
  try {
    written=navigator.clipboard.write([new Item({'text/plain':text})]);
  } catch(error) {
    console.warn('clipboard: copy refused',error);
    return null;
  }
  Promise.resolve(written).then(()=>report(true),error=>{
    if(given){report(false);return;}
    console.warn('clipboard: copy refused',error);
    report(false,COPY_REFUSED);
  });
  return value=>{
    if(typeof value==='string'){resolve(new Blob([value],{type:'text/plain'}));return;}
    given=true;
    reject(Error('nothing to copy'));
  };
}
