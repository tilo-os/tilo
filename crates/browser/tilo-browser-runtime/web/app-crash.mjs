/** What the page does when the runtime panics.
 *
 * The browser build aborts on a panic: the module cannot run again and the
 * page must start over. A panic inside a window is that window's application's
 * -- the runtime's hook names it -- so the next load shows that window's error
 * instead of running the application (`createCrashQuarantine`), and one
 * crashing app does not take every other window down with it on each load.
 *
 * A panic that names no window took the module down with no application to
 * blame, so the page itself is what must start over (`pageCrashAction`). It
 * does so once: the workspace, its windows and the unsent words in them are
 * records the session commits as it draws, so the load that follows shows
 * them again. A page that lands in the same crash right after would only
 * repeat it, so it stays put and says so (`showPageCrash`) instead. */
const KEY='tilo.crashed-windows.v1';
const LIMIT=32;
const TTL_MS=7*24*60*60*1000;
const MESSAGE_LIMIT=500;

export function createCrashQuarantine({storage=globalThis.localStorage,now=Date.now}={}) {
  const read=()=>{
    try{const list=JSON.parse(storage?.getItem(KEY)??'[]');return Array.isArray(list)?list:[];}
    catch{return [];}
  };
  const current=()=>read().filter(entry=>typeof entry?.room==='string'&&typeof entry.tab==='string'&&now()-entry.at<TTL_MS);
  const write=list=>{try{storage?.setItem(KEY,JSON.stringify(list.slice(-LIMIT)));}catch{}};
  return {
    /** `[{tab, message}]` for `workspace`, as `quarantine_windows` takes them. */
    entries(room){return current().filter(entry=>entry.room===room).map(({tab,message})=>({tab,message}));},
    /** Keep `tab` out of `workspace`'s next load. False when it already was: its
     * application ran anyway, and starting over would crash again. */
    record(room,{tab,message}){
      const list=current(),id=String(tab);
      if(list.some(entry=>entry.room===room&&entry.tab===id))return false;
      list.push({room,tab:id,message:String(message??'').slice(0,MESSAGE_LIMIT),at:now()});
      write(list);
      return true;
    },
    forget(room,tab){write(current().filter(entry=>!(entry.room===room&&entry.tab===String(tab))));},
  };
}

/** What the page does about a runtime panic `report` (`{tab, message}`, as
 * the runtime's hook sends it), for the window it names: start over without
 * that window once it is quarantined, else only report it (its application
 * would crash again on the next load). A panic that names no window is the
 * page's own: `pageCrashAction`. */
export function crashAction(report,recorded) {
  return report?.tab!=null&&recorded?'restart':'report';
}

/** The page crash an uncaught `error` stands for when the runtime's panic
 * hook never spoke (`panicked` false): a Wasm trap with no panic before it --
 * memory ran out, the stack overflowed -- leaves the module as dead as a panic
 * does, in the middle of whatever callback it was running. Every call after it
 * finds that callback still holding the runtime, so the page would stall
 * saying it is busy; it starts over like any panic outside a window instead.
 * `null` for anything else, and for the trap that ends a reported panic. */
export function trapCrash(error,{panicked}) {
  if(panicked||typeof WebAssembly==='undefined'||!(error instanceof WebAssembly.RuntimeError))return null;
  const stack=String(error.stack??'');
  const message=/rust_oom|handle_alloc_error/.test(stack)
    ?'the page ran out of memory'
    :`the runtime stopped: ${error.message}`;
  return {tab:null,message};
}

const RESTART_KEY='tilo.page-restarts.v1';
/** How long after the page started itself over a crash still counts as the
 * same one: a load younger than this that crashes again would only come back
 * into it. */
const RESTART_BOUND_MS=2*60*1000;

/** When the page last started itself over after a panic that named no window.
 * Kept like the crashed windows are, so a load that crashes right after finds
 * out it is on its second try. */
export function createPageRestarts({storage=globalThis.localStorage,now=Date.now,boundMs=RESTART_BOUND_MS}={}) {
  const last=()=>{
    try{const at=JSON.parse(storage?.getItem(RESTART_KEY)??'null')?.at;return Number.isFinite(at)?at:null;}
    catch{return null;}
  };
  return {
    /** Whether the page may start over now, recording that it does: false
     * while the load it would replace is itself younger than `boundMs`. */
    claim(){
      const at=last();
      if(at!==null&&now()-at<boundMs)return false;
      try{storage?.setItem(RESTART_KEY,JSON.stringify({at:now()}));}catch{}
      return true;
    },
  };
}

/** What the page does about a panic that named no window (`report`, as the
 * runtime's hook sends it): say what happened through the page's own error
 * route -- a panic outside every window must reach the door's log, not be
 * swallowed by the reload -- then start the page over. The load that follows
 * shows the workspace, its windows and their drafts as the session last
 * saved them; nothing the page holds in JS is worth more than that, the
 * runtime having kept the words as they were typed. When the page just did
 * this (`restarts`), `show` leaves a screen saying so instead. */
export function pageCrashAction(report,{restarts,reload,show,reportError}) {
  const message=String(report?.message??'').slice(0,MESSAGE_LIMIT);
  reportError?.('panic',`outside any window: ${message}`);
  if(restarts.claim()){reload();return 'reload';}
  show(message);
  return 'screen';
}

// The crash screen's palette: the first screen's, in the browser's own light
// or dark, so a dead page still looks like Tilo.
const CRASH_CSS='.tilo-page-crash{position:fixed;inset:0;z-index:2147483647;display:grid;place-items:center;box-sizing:border-box;padding:24px;background:#121214;color:#ececf0;font:15px/1.5 system-ui,-apple-system,\'Segoe UI\',sans-serif;text-align:center;}'
+'.tilo-page-crash-card{display:grid;gap:14px;justify-items:center;width:min(420px,100%);}'
+'.tilo-page-crash h1{margin:0;font-size:20px;font-weight:650;line-height:1.25;}'
+'.tilo-page-crash p{margin:0;color:#a8a8b2;overflow-wrap:anywhere;text-wrap:pretty;}'
+'.tilo-page-crash button{font:inherit;font-weight:500;min-height:44px;padding:0 20px;border:1px solid transparent;border-radius:10px;background:#f2f2f4;color:#121214;cursor:pointer;}'
+'.tilo-page-crash button:hover{background:#ffffff;}'
+'.tilo-page-crash button:focus-visible{outline:2px solid #9ab4ff;outline-offset:2px;}'
+'@media (prefers-color-scheme:light){.tilo-page-crash{background:#f7f7f9;color:#17181c;}'
+'.tilo-page-crash p{color:#55565f;}.tilo-page-crash button{background:#17181c;color:#ffffff;}'
+'.tilo-page-crash button:hover{background:#000000;}.tilo-page-crash button:focus-visible{outline-color:#2a3fd1;}}';

/** The screen the page leaves up when it cannot start over again: what
 * happened, and a Reload button that tries once more, for the person to press
 * when they choose. */
export function showPageCrash(message,{document:doc=globalThis.document,reload=()=>globalThis.location?.reload()}={}) {
  if(!doc?.body)return;
  const style=doc.createElement('style');
  style.textContent=CRASH_CSS;
  const screen=doc.createElement('div');
  screen.className='tilo-page-crash';
  // Announced as it appears: the page it covers stopped answering the person.
  screen.setAttribute('role','alert');
  const card=doc.createElement('div');
  card.className='tilo-page-crash-card';
  const title=doc.createElement('h1');
  title.textContent='Tilo hit a problem';
  const said=doc.createElement('p');
  said.textContent=String(message||'The application stopped.');
  const button=doc.createElement('button');
  button.type='button';
  button.textContent='Reload';
  button.addEventListener('click',()=>reload());
  card.append(title,said,button);
  screen.append(card);
  (doc.head??doc.body).append(style);
  doc.body.append(screen);
  try{button.focus();}catch{}
}
