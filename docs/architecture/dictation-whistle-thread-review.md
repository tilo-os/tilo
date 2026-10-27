# Whistle thread initialization

The twelve native tasks are an oversized compute pool, not twelve independently
identified support threads. On the inspected Linux runtime, the engine's default
constructor chooses its size from the host's online CPU count rather than the
admitted two-CPU affinity. The CLI has a supported explicit thread option; the
published native C API does not expose it. No budget was relaxed.

[Exact artifacts and measurements](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/architecture/dictation-whistle-thread-review.json) extend
the [release and paced-input review](dictation-whistle-paced-review.md). The
runtime identity remains [Needle3 revision 2ae11323](https://huggingface.co/Cactus-Compute/needle3/tree/2ae11323dc000f5e70c49f7403efa6af12ba9e67):
the earlier static archive and the distinct 3.2.0 shared library selected by the
[official wrapper](https://github.com/cactus-compute/needle/tree/af2654dffa4987f7e2cbbacdf59c2f854fd18302).

## Causal evidence

Read-only disassembly of the pinned archive identifies `whistle::Engine::Engine()`.
Its fallback calls libc++ `thread::hardware_concurrency()`, substitutes one when
needed, and caps the result at twelve before constructing `needle::ThreadPool`.
The pool creates workers numbered one through count minus one; the caller also
participates. Separate affinity/topology code chooses placement, but does not
constrain this fallback count.

The stripped 3.2.0 shared library contains the matching Whistle constructor
sequence at virtual address `0xc8dd5`, followed by pool construction at the same
engine field offset as the archive. Its concurrency target at `0x176970` calls
`sysconf(_SC_NPROCESSORS_ONLN)`. The builder reports 64 online CPUs; namespace
affinity admits two. This explains the observed twelve tasks even with that
affinity and a two-CPU quota. These are artifact-specific observations, not a
claim about all future releases or every platform.

Neither the archive's unresolved imports nor the shared library's dynamic imports
contain `getenv`. The earlier `OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS` and
`NEEDLE_THREADS` guesses were not supported configuration and did not constrain
this pool. The exact public header and eleven exported `needle_*` functions have
no thread setter. No private C++ entry point, interposer or binary patch was used.

An optional startup diagnostic reads only its own private namespace's task
accounting. Before the unchanged external guard stopped the process, it captured
the caller and three workers. All three workers were sleeping in `futex_do_wait`,
with 0.97–1.01 ms scheduled CPU time each. The archive's `worker_loop` shows
compute-job dispatch, initial spinning and condition-variable waiting. They are
idle compute workers at this snapshot, not demonstrated harmless support tasks.
The guard stopped observation before the remaining eight tasks could be sampled;
their states and future inference activity are not inferred.

## Budget and supported control

The production Whisper adapter accepts one or two **compute threads** and passes
that setting to recognition. The development observer's `max_threads=2` instead
counts **all native tasks** from `/proc`, independently of the scope's two-CPU
quota and 64-task ceiling. These are distinct contracts. The stricter review
guard remains in force; the compute pool cannot be exempted as support overhead.

A confined syscall trace of the official CLI decoding the existing public French
fixture with `--threads 2` records exactly one successful `CLONE_THREAD` creation,
whose worker is pinned to CPU 1. Its observed peak is two native tasks. Unlike the
C API default, this CLI setting avoids even a transient twelve-thread pool in
the recorded trace. Tracer overhead makes that run unsuitable for timing or CPU
comparisons.

A separate untraced, source-paced control changed only the supported CLI option
from two threads to one. All admission limits, model, public PCM, language,
single-pass decoding mode and cadence remained unchanged:

| French prefix control | Two threads | One thread |
| --- | ---: | ---: |
| First lexical output | 2.09 s, incorrect `Merci.` | 2.10 s, incorrect `Merci.` |
| First reference word matched, post-hoc | 3.08 s | 3.11 s |
| Completion after audio end | 2.20 s | 3.29 s |
| Measured decoder CPU time | 12.05 s | 7.92 s |
| Final raw word errors | 0 / 20 | 0 / 20 |

The one-thread run used eight decoder invocations, reached 41,080 KiB peak RSS
and 46,456 KiB peak virtual size, and produced no high/max/OOM events. It saved
CPU but worsened completion delay and retained the wrong early hypothesis. This
is one builder observation per setting, not a default change, a resident-stream
qualification or a fix for the physical laptop's first-word latency.

## Narrow native fix required

The verified public C API needs a pre-load thread option, applied when constructing
Whistle's engine and pool. An explicit count must respect the admitted affinity;
the automatic fallback should respect it too. Resizing after default construction
would still transiently violate the guard. Implementing that change requires the
exact licensed native source or a publisher-supported API; the separately audited
broader Cactus repository is not established as a compatible substitute.

The revised development fixture exposes `--startup-tasks` without changing normal
decoding. Native diagnostic refusals and one setup-path failure are retained;
only the CLI controls passed admission. All five new owned services are inactive.
No production runtime, publication, permission, model catalogue or microphone
boundary changed. The image and live microphone gates are separate from this
completed source/runtime investigation.

The [resident WebAssembly follow-up](dictation-whistle-wasm-review.md) implements
the publisher’s browser speech interface without the native startup pool. It
completes one bounded French stream but rejects interpreted execution for latency.
