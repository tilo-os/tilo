(()=>{
// Whether the entry page opens on its first screen (first-run.mjs), and at
// which step, inlined verbatim after the link scrubber so the screen is there
// at the first paint, before any module loads. It is decided by the person's
// setup this browser keeps (`tilo-setup`, first-run.mjs `SETUP`), and by
// nothing else: a setup under way opens on its step whatever else the page
// did meanwhile (joining the site's home workspace behind the screen, say),
// and one that is done never shows the screen again. That holds whatever
// workspace the address names: once the home workspace lets the page in it
// writes its own link there, and a reload must not take that for a link the
// person opened. A browser that keeps no setup sees the screen only when it
// is new here (no host let it in, no workspace entered) and opens the site
// itself (no route), and its setup starts now. A link with secrets (an
// invitation, a pairing) and a page inside another site never show it.
// Without storage the page opens as it always did.
var root=document.documentElement,secrets=globalThis.__tiloLinkSecrets||{};
if(Object.keys(secrets).length)return;
if(/[?&](embed|workspace|room|invite)(=|&|$)/.test(location.search))return;
var setup=null;
try{
 setup=JSON.parse(localStorage.getItem('tilo-setup')||'null');
 if(!setup||typeof setup!=='object'||typeof setup.step!=='string'){
  setup=null;
  if(location.hash.length>1&&location.hash!=='#/')return;
  var kept=['tilo-host-memberships','tilo-entered-workspaces'];
  for(var i=0;i<kept.length;i++)if(localStorage.getItem(kept[i]))return;
  setup={step:'start',at:Date.now()};
  localStorage.setItem('tilo-setup',JSON.stringify(setup));
 }
}catch(e){return;}
if(setup.step==='done')return;
var now=Date.now(),code=setup.code,offer=setup.offer,step=setup.step;
// A code that ran out while nobody looked has run out now.
if(step==='waiting'&&code&&!(setup.polled&&setup.polled.resolved)&&code.expiresAt<=now)step='expired';
if(step!=='waiting'&&step!=='expired'&&step!=='connected'&&step!=='opening')step='';
root.setAttribute('data-first-run',step==='opening'?'connected':step);
// What the screen showed last, painted once its markup is parsed
// (index.html calls this right after it), so a resumed setup shows its
// step at the first frame; first-run.mjs takes over and asks the host.
globalThis.__tiloSetupPaint=function(){
 var $=function(id){return document.getElementById(id);},screen=$('first-run'),shown=setup.shown||{};
 // The prompt breaks only between its words, as first-run.mjs `showPrompt` writes it.
 var put=function(id,text){var at=$(id);at.textContent='';String(text||'').split(/( +)/).forEach(function(part){if(!part)return;if(/^ +$/.test(part)){at.appendChild(document.createTextNode(part));return;}var word=document.createElement('span');word.className='fr-word';word.textContent=part;at.appendChild(word);});};
 if(!screen)return;
 var fresh=offer&&offer.expiresAt-now>60000&&shown.offer?shown.offer:'';
 screen.dataset.mode=shown.mode==='sign-in'?'sign-in':'setup';
 if(step===''){if(fresh){put('first-run-offer',fresh);$('first-run-offer-note').textContent='';}return;}
 if(step==='connected'||step==='opening'){
  screen.dataset.skipped=String(!shown.heading);
  $('first-run-connected').textContent=shown.heading||'Opening your workspace';
  if(!shown.heading)$('first-run-done-lead').textContent='';
  return;
 }
 screen.dataset.manual=String(setup.manual===true);
 if(step==='expired'){put('first-run-prompt',fresh);return;}
 put('first-run-prompt',shown.prompt);
 screen.dataset.copied=String(setup.copied===true);
 var lines=shown.lines||[],list=$('first-run-steps');
 for(var i=0;i<lines.length;i++){
  var line=lines[i],item=document.createElement('li');
  item.className=line.done?'done':line.act?'act':line.wait?'wait':line.failed?'failed':'now';
  item.textContent=line.text;list.appendChild(item);
 }
 if(code&&!(setup.polled&&setup.polled.resolved)){
  var minutes=Math.max(1,Math.ceil((code.expiresAt-now)/60000));
  $('first-run-left').textContent='Works once, for '+(minutes===1?'1 more minute':minutes+' more minutes')+'.';
 }
 $('first-run-net').textContent='Checking with '+location.host+'…';
};
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
