# Speech runtime compatibility with unchanged confinement

The pinned V8 worker fails before readiness because it requests
`pkey_mprotect(..., 8192, PROT_READ|PROT_WRITE|PROT_EXEC, 1)` and receives `EPERM`.
That is not evidence that all compiled speech engines require a policy exception.

The new opt-in `--policy-probe` uses **production** `worker::prepare` and
`speech::confine_worker`, including fully enforced Landlock, the parser filter,
parent-death termination and the existing 1 GiB address-space limit. It writes a
fixed reviewed x86-64 instruction fixture into an anonymous RW page, removes write
permission with `mprotect(..., PROT_READ|PROT_EXEC)`, and executes it. It then
requires ordinary RWX `mprotect` and the exact V8-shaped `pkey_mprotect` to fail
with `EPERM`. No model, V8 library or untrusted code enters this check.

The source-matched harness passes with one native thread, VmPeak 4988 KiB and
RSS 3188 KiB; no high/max/OOM events change. A separate trace corroborates the
permission transitions. A 100 ms hold keeps the completed child observable and
is **not** inference timing. Unsupported instruction-fixture architectures fail
rather than skip. A wrong executable receipt is rejected before starting the
child. Earlier controller syntax and missing-observation attempts remain failed.
The adjacent JSON preserves exact sources, executable and raw receipt hashes.

## Existing and prospective paths

| Path | Compatibility evidence | Decision |
|---|---|---|
| Existing pinned Node/V8 | Actual worker requests RWX and fails; its pinned code requests RWX even without protection keys | Reject this runtime under the current worker policy |
| Existing Wasmi interpreter | No JIT; the earlier public French run uses one thread but finishes in 46.055 s | Retain as interpreter evidence; interactive performance unqualified |
| Existing native Whisper | Production-confined benchmark receipts observe two total native threads; the bridge bounds compute threads to 1–2 and uses CPU code without JIT | Existing compatible fallback; recognition quality and physical latency remain separate limitations |
| Native Whistle archive/DSO | The earlier guard observes 12 tasks before audio; no supported public pre-initialization thread setter is exposed | Reject direct embedding; CLI `--threads 2` fits the budget but lacks the resident private speech-worker interface |
| Wasmtime 49.0.2 | Actual pinned module recognizes and streams after production confinement; source-built runtime publishes RX without RWX | [Default-off AOT qualification passes](dictation-whistle-aot-review.md); production activation remains unqualified |
| WABT wasm2c 1.0.42 | Supported Wasm-to-C output, including SIMD, compiled as ordinary native code | Alternative AOT proposal; actual module translation and performance untested |

No policy change is proposed. The repository's browser-specific JIT entitlements
are not a speech-worker entitlement. Do not switch this worker to `tilo-jit`, add
RWX exceptions, create writable aliases of executable pages, or move untrusted
model preparation outside confinement.

## Wasmtime proposal

Pin release commit `3c8a3e79aa3188a1a12b3aebbaa097abccceac92`. Its
[code-memory publisher](https://github.com/bytecodealliance/wasmtime/blob/3c8a3e79aa3188a1a12b3aebbaa097abccceac92/crates/wasmtime/src/runtime/code_memory.rs)
and [Unix mapping implementation](https://github.com/bytecodealliance/wasmtime/blob/3c8a3e79aa3188a1a12b3aebbaa097abccceac92/crates/wasmtime/src/runtime/vm/sys/unix/mmap.rs)
use an RW-to-RX transition. This matches the tested necessary kernel property;
it does not establish compatibility of every initialization syscall.

Compile the exact receipt-checked speech module **at build time** with
`Engine::precompile_module`, producing an architecture/configuration-specific
immutable runtime artifact. This is different from moving V8 initialization
before the worker's filter. Load the trusted artifact after applying the existing
worker confinement and before parsing untrusted model weights.

[Deserialization is unsafe for arbitrary artifacts](https://github.com/bytecodealliance/wasmtime/blob/3c8a3e79aa3188a1a12b3aebbaa097abccceac92/crates/wasmtime/src/runtime/module.rs):
only authenticated exact compiler output is admissible. Bind the module hash,
engine version, target/CPU features, complete compilation configuration, size,
digest and notices into the existing immutable runtime release provenance. Use
an immutable snapshot, not a mutable user cache or model-supplied native blob.
CPU-incompatible output must fail before execution; match the repository OS
floor deliberately rather than inheriting the builder's ISA. The existing native
[CPU build](../../crates/agent/local-model-service/native/CMakeLists.txt) uses
x86-64-v3/armv8-a baselines and disables runner-native, OpenMP and GPU backends.
Its [historical confined baseline receipts](dictation-baseline-qualification.json)
observe two total threads; this follow-up does not claim a new acoustic benchmark.

The [configuration](https://github.com/bytecodealliance/wasmtime/blob/3c8a3e79aa3188a1a12b3aebbaa097abccceac92/crates/wasmtime/src/config.rs)
permits disabling parallel compilation and setting memory reservation, guard and
growth reservation to zero, avoiding the default 4 GiB reservation. Qualify an
on-demand allocator with a hard 512 MiB guest-memory limit inside the unchanged
1 GiB process cap. Keep signal-trap and Spectre protections enabled: this version
requires disabling Spectre mitigations when signal traps are disabled. Do not
use that shortcut or claim these memory settings already passed a Whistle run.

Keep the existing private host-import allowlist; no WASI filesystem, sockets,
processes, ambient handles, async executor or background timer is needed.
Owned-process cancellation remains the existing pipe/process termination path.
Map the prepared engine to `SpeechDecoder`; readiness, S16LE sequencing and one
literal endpoint commit stay with the existing worker and native editing path.
Wasmtime is not in this workspace lockfile or reviewed installed runtime; no
dependency, binary, package, catalogue entry or trust grant was added here.

## wasm2c alternative

Pin WABT commit `ff0ef7e0009402740c805a9744c09b05be063e48`.
The [feature matrix](https://github.com/WebAssembly/wabt/blob/ff0ef7e0009402740c805a9744c09b05be063e48/README.md)
includes SIMD, while [the writer](https://github.com/WebAssembly/wabt/blob/ff0ef7e0009402740c805a9744c09b05be063e48/src/c-writer.cc)
emits SIMDe operations. Translation still must accept the exact pinned module;
relaxed SIMD and unsupported features must fail instead of being approximated.
Package generated code with the reviewed host-import adapter and runtime/notices
as immutable native code, not a model-provided library.

Use the documented explicit bounds-check memory strategy with bounded allocation
and stack accounting, not a multi-gigabyte guard reservation. Retain the
[documented compiler conformance flags](https://github.com/WebAssembly/wabt/blob/ff0ef7e0009402740c805a9744c09b05be063e48/wasm2c/README.md)
and do not disable bounds, trap or floating-point correctness checks. Enforce
the 512 MiB guest ceiling on every allocation/growth path. Ordinary native code
needs no RWX mapping, but actual generation, import matching, traps, resident
streaming and end-to-end resource/performance qualification remain unrun.

## Next acceptance boundary

A source-only engine experiment must use the pinned module/model/public PCM and
existing admission/observer limits, initialize **inside** production confinement,
and trace memory operations to reject every RWX request. Observe all native
threads throughout load and decode, not only a compute setting. Verify bounded
memory/growth, malformed imports and artifact/CPU/configuration rejection before
readiness. Then exercise the real private pipe: literal endpoint, silence,
unfinished/canceled sessions, repeated sessions, language/dictionary revocation
and the already-developed native original-field insertion/undo path.

Keep all candidates default-off until that evidence exists. The kernel probe is
a narrow positive policy check, not a successful Wasm execution, decoder
benchmark, recognition acceptance or permission to deploy. The historical V8
worker receipts reproduce at commit `468549391f11117bdcd028ee2e0bbce34b8a16a7`;
this follow-up preserves rather than rewrites their rejected source profile.

Validation: strict model-service all-target Clippy with the review feature passed
(0.63 s), two final-source custody tests passed with zero skips, and the required
shell `--tests` compile passed (1.08 s). Python Ruff/syntax and source-receipt
checks passed. All seven owned compatibility scopes are inactive. No compiler or
runtime was installed, no catalogue or permission changed, and no device or
session was restarted.
