(()=>{
// Whether the entry page opens on its first screen (first-screen.mjs), and at
// which step, inlined verbatim after the link scrubber so the screen is there
// at the first paint, before any module loads. It is decided by the person's
// setup this browser keeps (`tilo-setup`, first-screen.mjs `SETUP`), and by
// nothing else: a setup under way opens on its step whatever else the page
// did meanwhile (joining the site's home workspace behind the screen, say),
// and one that is done never shows the screen again. That holds whatever
// workspace the address names: once the home workspace lets the page in it
// writes its own link there, and a reload must not take that for a link the
// person opened. A browser that keeps no setup sees the screen only when it
// is new here (no host let it in, no workspace entered) and opens the site
// itself (no route), and its setup starts now. A link with secrets (an
// invitation, a pairing) and a page inside another site never show it.
// A newcomer who arrives by an invitation joins that workspace first, with
// nothing in their way: their own setup is kept for later (`later`), and
// shows on the site itself only while they have joined no workspace; once
// they have, that workspace offers it after some use
// (identity-actions.mjs), never between them and their work. A browser this
// site knows nothing of is not shown
// the setup ahead of its entry's answer: a site that takes people by
// invitation only may send them to its wait list, and the setup is no way
// in where they hold no invitation. The page shows the screen itself once
// the entry let them in (first-screen.mjs `reveal`). Without storage the page
// opens as it always did.
var root=document.documentElement,secrets=globalThis.__tiloLinkSecrets||{};
var fresh=function(){
 var kept=['tilo-host-memberships','tilo-entered-workspaces'];
 for(var i=0;i<kept.length;i++)if(localStorage.getItem(kept[i]))return false;
 return true;
};
var bare=!(location.hash.length>1&&location.hash!=='#/');
if(Object.keys(secrets).length){
 try{if(secrets.invite&&!localStorage.getItem('tilo-setup')&&fresh())localStorage.setItem('tilo-setup',JSON.stringify({step:'later',at:Date.now()}));}catch(e){}
 return;
}
if(/[?&](embed|workspace|workspaceName|invite)(=|&|$)/.test(location.search))return;
var setup=null;
try{
 setup=JSON.parse(localStorage.getItem('tilo-setup')||'null');
 if(!setup||typeof setup!=='object'||typeof setup.step!=='string'){
  setup=null;
  if(!bare||!fresh())return;
  // Nothing this browser kept: no setup, no host that let it in, no
  // workspace it entered. Whether the setup is even theirs to take up is
  // the site's entry's to say (it may keep a wait list instead), so the
  // screen is wired but held, and the page shows it once it is in.
  root.setAttribute('data-first-screen-hold','');
  return;
 }else if(setup.step==='later'){
  // Shown on the site itself, and kept for later still: until they take
  // it up, a workspace's address opens the workspace. A browser that has
  // already joined a workspace opens it on the site itself too: their own
  // setup waits inside the workspace, which offers it once they have used
  // it a while (identity-actions.mjs), and never stands between them and
  // the work they came back to.
  if(!bare||!fresh())return;
  setup={step:'start',at:setup.at};
 }
}catch(e){return;}
if(setup.step==='done')return;
var now=Date.now(),code=setup.code,offer=setup.offer,step=setup.step;
// A code that ran out while nobody looked has run out now.
if(step==='waiting'&&code&&!(setup.polled&&setup.polled.resolved)&&code.expiresAt<=now)step='expired';
if(step!=='waiting'&&step!=='expired'&&step!=='connected'&&step!=='opening')step='';
root.setAttribute('data-first-screen',step==='opening'?'connected':step);
// What the screen showed last, painted once its markup is parsed
// (index.html calls this right after it), so a resumed setup shows its
// step at the first frame; first-screen.mjs takes over and asks the host.
globalThis.__tiloSetupPaint=function(){
 var $=function(id){return document.getElementById(id);},screen=$('first-screen'),shown={};
 // What the screen showed last, kept apart from the setup (first-screen.mjs `SHOWN`), for the code it was drawn for.
 try{var drawn=JSON.parse(localStorage.getItem('tilo-setup-shown')||'null');if(drawn&&typeof drawn==='object'&&(drawn.code||null)===((code&&code.code)||null))shown=drawn;}catch(e){}
 // The prompt breaks only between its words, as first-screen.mjs `showPrompt` writes it.
 var put=function(id,text){var at=$(id);at.textContent='';String(text||'').split(/( +)/).forEach(function(part){if(!part)return;if(/^ +$/.test(part)){at.appendChild(document.createTextNode(part));return;}var word=document.createElement('span');word.className='fr-word';word.textContent=part;at.appendChild(word);});};
 if(!screen)return;
 var fresh=offer&&offer.expiresAt-now>60000&&shown.offer?shown.offer:'';
 screen.dataset.mode=shown.mode==='sign-in'?'sign-in':'setup';
 if(step===''){if(fresh){put('first-screen-offer',fresh);$('first-screen-offer-note').textContent='';screen.dataset.setup='ready';}return;}
 if(step==='connected'||step==='opening'){
  screen.dataset.skipped=String(!shown.heading);
  $('first-screen-connected').textContent=shown.heading||'Opening your workspace';
  if(!shown.heading)$('first-screen-done-lead').textContent='';
  return;
 }
 screen.dataset.manual=String(setup.manual===true);
 if(step==='expired'){put('first-screen-prompt',fresh);return;}
 put('first-screen-prompt',shown.prompt);
 screen.dataset.copied=String(setup.copied===true);
 // An agent took the code without a copy: the heading says what it is doing.
 screen.dataset.handed=String(setup.handed===true);
 var who=(setup.polled&&setup.polled.agent)||'Your agent',names=screen.querySelectorAll('.fr-hand-agent');
 for(var j=0;j<names.length;j++)names[j].textContent=who;
 var lines=shown.lines||[],list=$('first-screen-steps');
 for(var i=0;i<lines.length;i++){
  var line=lines[i],item=document.createElement('li');
  item.className=line.done?'done':line.act?'act':line.wait?'wait':line.failed?'failed':'now';
  item.textContent=line.text;list.appendChild(item);
 }
 if(code&&!(setup.polled&&setup.polled.resolved)){
  var minutes=Math.max(1,Math.ceil((code.expiresAt-now)/60000));
  $('first-screen-left').textContent='Works once, for '+(minutes===1?'1 more minute':minutes+' more minutes')+'.';
 }
 $('first-screen-net').textContent='Checking with '+location.host+'…';
};
// The screen is here before its module is: a press on one of its buttons
// before then is kept, and the module presses it again once it can answer,
// so a person who clicks at once is not ignored.
var early=globalThis.__tiloFirstScreenEarly=[];
document.addEventListener('click',function(e){
 if(globalThis.__tiloFirstScreenReady)return;
 var b=e.target&&e.target.closest&&e.target.closest('#first-screen button[id]');
 if(b)early.push(b.id);
},true);
})();
