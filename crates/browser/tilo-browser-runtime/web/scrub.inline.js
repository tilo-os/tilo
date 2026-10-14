(()=>{
// The entry pages' first script, inlined verbatim into each so it runs
// before any module loads: a link's secrets (invite, ticket, token, pair,
// install) leave the address bar here, so copying the URL, a crash report
// or a later share-link never sees them. They stay in page memory for admission to
// read once. This only splits off the five known keys; the Rust parser
// reads everything else. Secrets that are not the trailing parameters in
// grammar order are still removed, and flagged, so admission refuses the
// link instead of acting on one Tilo did not write.
var hash=location.hash,mark=hash.indexOf('?'),found={},order=['invite','ticket','token','pair','install'],last=-1,bad=false;
if(mark>=0){
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
