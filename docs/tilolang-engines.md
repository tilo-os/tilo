# tilolang engines: tiered, wasmi, Cranelift and Winch

tilolang apps run on the tiered engine (`crates/lang/tilolang-runtime/DECISIONS.md`, "Tiered"):
a module starts on wasmi, an interpreter, while Cranelift, wasmtime's optimizing compiler,
compiles it on a thread of its own; the next call after the compile finishes runs the compiled
module, and the compiled module is kept on disk so the next start is compiled at once. A
process can also run on one engine alone, wasmi, Cranelift or Winch (wasmtime's baseline
compiler), to compare them on your own machine. wasmtime is the runtime's `wasmtime` feature,
on by default; a build without it runs wasmi alone.

## Running the comparison

```
python3 tools/perf/tilolang_engines.py
```

It builds `tilolang bench` (the `bench` example of `tilolang-tilo`) in release and the image
filter library
(`crates/platform/tilolang-tilo/bench/imagefilter/build.sh`). Then it runs the bench once
per engine over every app in `apps/tl` at 1k and 10k items and prints one table of p50s.
Options: `--engines tiered,wasmi`, `--apps notes,todo`, `--sizes 1000`, `--samples 20`,
`--records out.jsonl` (every record, as `tools/perf/record.py` JSON), and `--no-build` to rerun
on the binaries already built. The whole run takes about 15 minutes. Close anything heavy first:
the numbers are wall-clock times.

One engine on one app:

```
cargo build --release --locked -p tilolang-tilo --example bench
target/release/examples/bench apps/tl/notes --engine cranelift --sizes 1000,10000 \
    --compute --library target/wasm32-unknown-unknown/release/tilolang_bench_imagefilter.wasm
```

## What it measures

- **compile**: compiling the app's modules in a process that has not compiled them yet. For
  wasmi this is validation only: lazy translation translates each function on its first call,
  which the start-up figure includes.
- **compile, peak memory**: how much a fresh process's peak resident memory rises while it
  compiles the modules and makes their first instance. It includes the engine's one-time setup
  in that process and, for wasmi, the instance's linear memory, which wasmi fills with zeros.
- **start to first frame**: opening the app's main view to its first tree in a process that
  has compiled nothing yet, compiling included (on the tiered engine, with nothing kept on disk
  either: the first frame comes from wasmi).
- **start to first frame, compiled module kept** (tiered only): the same in a process that
  finds the compiled modules on disk, as every start after the first does.
- **open, keystroke, press, peer edit, send in a thread**: the bench's usual scenarios
  (`tilolang bench`), with the modules already compiled (on the tiered engine, after its
  compile finished: what an app runs a moment after it starts).
- **compute**: `crates/platform/tilolang-tilo/bench/compute`, a tilolang package doing 64-bit
  integer arithmetic over 2^20 indices. tilolang has no buffers and no counting loops, so it runs
  this twice: as recursive guest calls only, and as four nested `for` loops over a 32-element
  list. In the loops, each step reads an element through the host (`list_get_i`).
- **library**: `crates/platform/tilolang-tilo/bench/imagefilter`, a 3x3 box blur written in Rust
  and compiled for `wasm32-unknown-unknown`, run on a 512 x 512 RGBA image for 4 passes. It is
  loaded and called through the runtime's engine like a library an app ships.

Every workload's result is in its record (`result`, `checksum`), and the script checks that all
engines agree.

## Measured: the tiered engine

On the arm64 rcargo builder (`rcargo exec`, shared with other builds, so a few percent is
noise), release, 10 samples, 1k items (Chat: 20 conversations, its thread 1k messages), one
binary for every engine. p50s in ms. *wasmi before* is the same scenarios on the binary of
integration a01f8006bee, where wasmtime imports and calls converted every value through `Val`.

| measure | wasmi before | Cranelift before | tiered | wasmi | Cranelift | Winch |
|---|---:|---:|---:|---:|---:|---:|
| compile, tasks | 0.217 | 63.1 | 0.239 | 0.219 | 14.1 | 2.94 |
| compile, notes | 0.109 | 11.8 | 0.123 | 0.114 | 11.5 | 1.72 |
| start to first frame (install, nothing compiled), notes | 26.4 | 38.9 | 26.1 | 26.7 | 37.4 | 27.4 |
| start to first frame (install), tasks | 43.1 | 59.6 | 43.8 | 44.5 | 57.1 | 45.6 |
| start to first frame (install), chat | 2.29 | 10.8 | 2.76 | 2.44 | 10.8 | 4.21 |
| start to first frame (install), shell bar | | | 14.9 | 15.8 | 18.3 | 15.1 |
| start to first frame, compiled module kept, notes / tasks / shell | | | 25.9 / 44.0 / 14.5 | | | |
| first frame over the index, tasks | 9.43 | 11.4 | 8.20 | 9.75 | 8.27 | 8.29 |
| keystroke, notes (typing) | 1.40 | 1.46 | 1.33 | 1.42 | 1.35 | 1.37 |
| keystroke, tasks | 1.95 | 2.33 | 1.89 | 2.11 | 1.92 | 1.85 |
| peer edit, tasks | 1.24 | 1.50 | 1.20 | 1.33 | 1.23 | 1.18 |
| peer edit, shell bar (the session changes the desk) | | | 0.624 | 0.635 | 0.623 | 0.639 |
| send in a thread, chat | 6.43 | 6.69 | 6.57 | 6.74 | 6.80 | 6.33 |
| compute, guest calls (2^20) | 104 | 14.3 | 14.3 | 105 | 14.3 | 23.9 |
| compute, loops over a list (2^20) | 216 | 231 | 77.5 | 212 | 77.6 | 95.9 |

The shell bar has nothing to type into, so its change is the peer edit; the bench of
integration a01f8006bee could not run it. The bench has no "add a task" scenario: a task's
change is its keystroke and its peer edit.

- **Launch.** The tiered engine starts like wasmi (it is wasmi until the compile ends), about
  10 to 15 ms sooner than Cranelift alone, for which a launch waits on the compile. With the
  compiled module kept on disk the start is the same: loading it costs what wasmi's lazy
  validation does, and the first frame's time is in the host.
- **Steady state.** A few milliseconds after load the tiered engine runs Cranelift's code:
  views and changes are 3 to 15% faster than on wasmi, guest arithmetic 7x, and code that calls
  the host in a loop 2.7x. Before, that loop was slower on Cranelift than on wasmi (231 against
  216 ms) because every import converted its values through `Val`; raw values (`ValRaw`, checked
  against the export's type once) removed that.
- The results agreed on every engine.

## Measured: the engines alone (October 2026)

On the Lenovo Yoga laptop (aarch64, 12 threads), release, 10 samples, wasmtime 50.0.0 (which
has the fix for CVE-2026-34987, the Winch-on-aarch64 sandbox escape fixed in 36.0.7, 42.0.2 and
43.0.1). p50s in ms unless stated.

| measure | wasmi | Cranelift | Winch |
|---|---:|---:|---:|
| compile, notes | 0.048 | 2.60 | 0.906 |
| compile, board (largest module) | 0.159 | 10.2 | 4.02 |
| compile, image filter library | 0.061 | 3.36 | 0.989 |
| compile peak memory, notes (KiB) | 1908 | 9128 | 31128 |
| compile peak memory, library (KiB) | 11104 | 9276 | 30448 |
| start to first frame, counter | 0.179 | 1.26 | 0.885 |
| start to first frame, notes 1k | 8.11 | 12.4 | 9.72 |
| start to first frame, notes 10k | 29.6 | 42.9 | 41.4 |
| start to first frame, todo 10k | 40.6 | 77.5 | 45.3 |
| start to first frame, board 10k | 101 | 123 | 202 |
| open, notes 10k | 22.0 | 22.2 | 21.0 |
| open, letters 10k | 112 | 125 | 121 |
| keystroke, notes 10k | 0.332 | 0.391 | 0.326 |
| keystroke, board 10k | 0.528 | 0.567 | 0.588 |
| peer edit, notes 10k | 0.196 | 0.196 | 0.201 |
| peer edit, board 10k | 0.312 | 0.329 | 0.324 |
| compute, guest calls (2^20) | 122 | 15.3 | 24.7 |
| compute, loops over a list (2^20) | 161 | 187 | 214 |
| library, blur 512x512 x4 | 603 | 11.7 | 62.3 |

The results agreed on every engine. The script prints every app at both sizes. Across the app
scenarios (open, keystroke, press, peer edit) the three engines were within a few percent of
each other, and wasmtime was more often slower than faster.

The release bench binary is 7.6 MiB stripped without wasmtime and 15.0 MiB with it, Cranelift
and Winch both: the feature adds 7.4 MiB (9.9 to 19.7 MiB unstripped).

What the numbers say:

- For UI code, the engine makes no difference: an app's time is in the host. Compiling first
  makes a cold start slower: Cranelift adds about 1 to 37 ms per launch, Winch 1 to 100 ms
  (board at 10k). Winch also raises a fresh process's peak memory by about 30 MiB, against
  9 MiB for Cranelift.
- Guest arithmetic runs 8x faster on Cranelift than on wasmi, and 5x faster on Winch. The Rust
  library runs 52x faster on Cranelift and 10x faster on Winch.
- Code that calls the host in a tight loop was 15 to 33% slower on wasmtime: each import
  converted its values through `Val`. Raw values fixed it (the tiered table above).

## Running the suites on another engine

The runtime's suites run on any engine through `TILOLANG_ENGINE`, which a build with the
`wasmtime` feature reads:

```
cargo test --locked -p tilolang-runtime -p tilolang-tilo \
    --config 'env.TILOLANG_ENGINE="winch"'
```

`--config env.…` gives the variable to the tests through cargo. A test fails if the engine running is not the one named.

## The seam

`crates/lang/tilolang-runtime/src/engine.rs` holds everything the runtime does with wasm,
written once: the per-process module cache, the instance pools, the profile, import dispatch and
the text of every error. What differs between engines is a private trait, `Backend`, with one
implementation per engine (`engine/wasmi_backend.rs`, `engine/wasmtime_backend.rs`):

- `compile(bytes) -> Module`, a module every runtime in the process shares;
- `exports(module)`;
- `linker()`: the `"tl"` import table bound to the shared import function, one per runtime;
- `instantiate(linker, module, limits) -> Instance`: a store with fuel for one call and the
  memory limit, the start function run;
- `call(instance, export, args, fuel, cx)`: one call, with the host and profile in the store for
  its duration.

`configure(EngineConfig { kind, translation })` picks the engine once per process, before the
first runtime; `keep_compiled_in(dir)` says where the tiered engine keeps compiled modules.

The tiered engine (`engine/tiering.rs`) uses both backends: each runtime holds a wasmi pool and
a Cranelift pool per module, and a call checks out an instance from the Cranelift pool once the
process's compile thread has the compiled module, dropping the wasmi ones. A call running when
the compile finishes ends on wasmi; nothing waits.
