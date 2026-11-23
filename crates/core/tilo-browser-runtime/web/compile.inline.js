(()=>{
// The entry pages' start of the runtime, inlined verbatim into each right
// after the link scrubber, before any module loads. The runtime module is
// most of a first visit's download. Left to the page's modules (host.mjs
// calls init once they all loaded and ran), its compile starts only then;
// streamed from here it compiles while it downloads and while the modules
// load. host.mjs takes the compiled module; where this did not run or
// failed, init fetches and compiles the module itself as before.
try{
 var hint=document.querySelector('link[rel="preload"][href$="tilo_browser_runtime_bg.wasm"]');
 if(!hint||typeof WebAssembly!=='object'||typeof WebAssembly.compileStreaming!=='function')return;
 // The preload's own request (cors, same-origin credentials), so this takes
 // its response rather than downloading the module a second time.
 var compiled=WebAssembly.compileStreaming(fetch(hint.href,{mode:'cors',credentials:'same-origin'}));
 compiled.catch(function(){});
 globalThis.__tiloStartModule=compiled;
}catch(error){}
})();
