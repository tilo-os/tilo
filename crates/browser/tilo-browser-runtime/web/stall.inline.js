(()=>{
// The entry pages' way out while their modules load, inlined verbatim into
// each after the link scrubber: before the arrival screen (arrival.mjs)
// runs, nothing else on the page can say that loading stopped. A module
// that fails to load (a dropped connection, a deploy replacing the files)
// stops the page: it says so, and Try again reloads it. One still not
// running after a while offers Try again while it keeps loading.
var root=document.documentElement,wait=20000;
addEventListener('error',function(event){
 var target=event.target;
 if(!target||target.tagName!=='SCRIPT'||root.dataset.arrivalProgress!==undefined)return;
 root.dataset.arrival='stopped';root.dataset.entry='failed';
 var status=document.getElementById('status');
 if(status)status.textContent='Tilo could not load. Check your connection, then try again.';
},true);
setTimeout(function(){if(root.dataset.arrivalProgress===undefined)root.setAttribute('data-arrival-stalled','');},wait);
addEventListener('click',function(event){if(event.target&&event.target.id==='arrival-retry')location.reload();});
})();
