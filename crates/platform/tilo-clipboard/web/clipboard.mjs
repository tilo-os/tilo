// The clipboard backend (src/web.rs) needs a synchronous publication result
// (Cut must not delete text after a denied write). Reuse the browser's native
// copy event. Async Clipboard.writeText cannot satisfy that contract.
// `report(true)` or `report(false, reason)` hears the outcome.
export function writeClipboardText(text, report, document=globalThis.document) {
  let written=false, failure;
  const transfer=event=>{
    // Do not reenter the WASM keyboard handler while it is dispatching a menu.
    event.stopImmediatePropagation();
    event.preventDefault();
    try {
      if(!event.clipboardData)throw Error('Clipboard transfer is unavailable');
      event.clipboardData.setData('text/plain',text);
      written=true;
    } catch(error) { failure=error; }
  };
  try {
    if(!document?.execCommand)throw Error('System clipboard is unavailable');
    document.addEventListener('copy',transfer,{capture:true});
    const completed=document.execCommand('copy');
    if(!completed||!written||failure)throw failure??Error('Browser refused clipboard access. Try the keyboard Copy shortcut.');
    report(true);
    return true;
  } catch(error) {
    report(false,String(error));
    return false;
  } finally {
    document?.removeEventListener('copy',transfer,{capture:true});
  }
}
