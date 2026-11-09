(()=>{
// Whether the entry page opens on its first screen (first-run.mjs), inlined
// verbatim after the link scrubber so the screen is there at the first
// paint, before any module loads. Only a browser new here sees it: one that
// keeps nothing from an earlier visit (no first screen seen, no host that
// let it in, no workspace entered), opening the site itself rather than a
// link (no secrets, no route, no workspace named) and not inside another
// site. Without storage the page opens as it always did.
var root=document.documentElement,secrets=globalThis.__tiloLinkSecrets||{};
if(Object.keys(secrets).length)return;
if(location.hash.length>1&&location.hash!=='#/')return;
if(/[?&](embed|workspace|room|invite)(=|&|$)/.test(location.search))return;
try{
 var kept=['tilo-first-run','tilo-host-memberships','tilo-entered-workspaces'];
 for(var i=0;i<kept.length;i++)if(localStorage.getItem(kept[i]))return;
}catch(e){return;}
root.setAttribute('data-first-run','');
// The screen is here before its module is: a press on one of its buttons
// before then is kept, and the module presses it again once it can answer,
// so a person who clicks at once is not ignored.
var early=globalThis.__tiloFirstRunEarly=[];
document.addEventListener('click',function(e){
 if(globalThis.__tiloFirstRunReady)return;
 var b=e.target&&e.target.closest&&e.target.closest('#first-run button[id]');
 if(b)early.push(b.id);
},true);
})();
