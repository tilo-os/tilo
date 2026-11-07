(()=>{
// The entry pages' first script, inlined verbatim into each so it runs
// before any module loads: a link's secrets (invite, ticket, token, pair,
// install, link) leave the address bar here, so copying the URL, a crash report
// or a later share-link never sees them. They stay in page memory for admission to
// read once. This only splits off the six known keys; the Rust parser
// reads everything else. Secrets that are not the trailing parameters in
// grammar order are still removed, and flagged, so admission refuses the
// link instead of acting on one Tilo did not write.
var hash=location.hash,mark=hash.indexOf('?'),found={},order=['invite','ticket','token','pair','install','link'],last=-1,bad=false;
// A sealed invitation's link carries its key alone after the `#`, at the
// site's `/i/<handle>` (sealed-link.mjs): it is the invitation's `invite`,
// and leaves the address the same way.
var sealed=/^\/i\/[A-Za-z0-9_-]{16}$/.test(location.pathname)&&/^#[A-Za-z0-9_-]{43}$/.test(hash);
if(sealed){
 found.invite=hash.slice(1);
 history.replaceState(history.state,'',location.pathname+location.search);
}else if(mark>=0){
 var keep=[],parts=hash.slice(mark+1).split('&');
 for(var i=0;i<parts.length;i++){
  var part=parts[i],eq=part.indexOf('='),name=eq<0?part:part.slice(0,eq),slot=order.indexOf(name);
  if(slot<0){if(last>=0)bad=true;keep.push(part);continue;}
  if(slot<=last)bad=true;
  last=slot;found[name]=part.slice(eq+1);
 }
 if(last>=0){
  history.replaceState(history.state,'',location.pathname+location.search+hash.slice(0,mark)+(keep.length?'?'+keep.join('&'):''));
 }
}
if(bad)found.malformed=true;
Object.defineProperty(globalThis,'__tiloLinkSecrets',{value:Object.freeze(found),configurable:true});
})();
