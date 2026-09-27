// The instantiation half of the glue wasm-bindgen 0.2.108 generates with
// `--target web`, as it is emitted for the core (typed-array caches and
// bindings left out). The module stays in a private variable: 0.2.108 sets no
// `__wbindgen_wasm_module`, which is why the host compiles the core itself.
let wasmModule, wasm;
function __wbg_get_imports() {
    return {__proto__: null};
}
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    wasm.__wbindgen_start();
    return wasm;
}
async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);
                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);
                } else { throw e; }
            }
        }
        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);
        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }
    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}
async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;
    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }
    if (module_or_path === undefined) {
        module_or_path = new URL('tilo_browser_runtime_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();
    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }
    const { instance, module } = await __wbg_load(await module_or_path, imports);
    return __wbg_finalize_init(instance, module);
}
export const loaded = () => wasmModule;
export { __wbg_init as default };
