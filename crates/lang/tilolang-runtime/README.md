# tilolang-runtime

The WebAssembly host that runs compiled tilolang packages. It loads a module the
[tilolang](https://crates.io/crates/tilolang) compiler produced, builds resources, invokes their
actions, evaluates programs and renders views.

The runtime owns the machine beside the language: the handle table and the `"tl"` import ABI,
the action transaction and its run journal, keyed view trees and the patches that update them,
an Automerge-backed reference store, and invocation accounting per action and per view. It does
not parse or check tilolang source; the compiler does that, and the runtime loads the result
under the package id it was signed as.

The Tilo session and its app host use this crate to run tilolang apps. It is also the crate to
embed when another host, tool or test wants to run a compiled package: a package author writing
in tilolang never depends on it, and an app written against the Rust SDK does not either.

## Use it

```toml
[dependencies]
tilolang-runtime = "0.4"
```

```rust
use tilolang_runtime::{Actor, AmStore, MemberId, Runtime, SimpleAuthority, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The module `tilolang` compiled for one package.
    let wasm = std::fs::read("counter.wasm")?;
    let person = Actor::from(MemberId::person("me"));

    let runtime = Runtime::new(AmStore::new(), SimpleAuthority::new())?;
    runtime.load_wasm(&wasm)?;

    let counter = runtime.create("Counter")?;
    runtime.invoke_action(&person, ("increment", vec![Value::resource(&counter)]))?;

    println!("{:?}", runtime.read(&counter, "value")?);
    Ok(())
}
```

`Runtime::new` takes a `Store` and an `Authority`. `AmStore` keeps documents in memory and
`SimpleAuthority` decides by module authorship; a platform supplies its own implementations of
both. `Runtime::load_package` takes a module with its `Meta` when the caller already holds the
metadata. `Runtime::open_view` opens a view and returns the `ViewInstance` that receives the
person's events and edits. `Runtime::record_costs` turns counting on and `Runtime::costs`
returns what the calls since then spent in the language's own units.

## Learn more

- Language documentation: https://tilolang.dev/docs/
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
