# ush without a Linux underneath

Commands run where they can, and a container or VM starts only where a
command needs one.

Status: decided 2026-10-28 (see [Decisions](#decisions)), built in slices
(see [Order of work](#order-of-work)). The shell and tier 1 are in
[`ush.md`](ush.md), "The session's shell".

## Problem

Today every ush command, even `ls`, needs a real shell on a real Linux: a
bwrap guest on the person's own device, or a guest on one of the person's own
relays ([`ush.md`](ush.md), "Environments on a relay"). So:

- a workspace hosted elsewhere (tilo.coffee, a browser tab, a phone) can't
  run any command at all;
- even `cat notes.md` pays for a whole Linux sandbox;
- what a command changes lives outside Tilo's history unless ush rebuilds it.
  The file commands already don't: they are the environment's actions
  ([`ush.md`](ush.md), "`cp`, `mv`, `rm` … the environment's actions").

Shell commands should work anywhere a workspace is open. Each command runs in
the cheapest place that can run it, and a container or VM starts only for the
commands that need Linux, and only for as long as they need it.

## What there is to build on

- **File commands are actions.** `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `ln`,
  `truncate`, `touch` and `append` are the environment record's own verbs
  ([`tilo_environment::files`](../../crates/runtime/environment/src/files.rs)):
  checked as a whole against the record's grants, queued in its `files`
  outbox for the device whose disk they change, and undone from its History.
- **Files are versioned.** Every write to a Tilo mount appends a version, and
  `version` reads that history.
- **Command runs are records.** Terminal's runs are records with an
  `undoable` flag, and `run`, `rerun` and `rerun-test` are actions.
- **Sandboxed WebAssembly.** wasmi is already a workspace dependency, and runs
  on devices and hosts alike.
- **Compute jobs that run where it's best.** Offloadable actions and compute
  jobs (`tilo_relay::offload`, `tilo_relay::compute`) place a job on the person's best
  device, or run it locally. A job names a signed package and an entrypoint,
  takes its inputs by digest, and its outputs come back as workspace content.
  It never carries a shell string or a host path.

## Three tiers, one shell

Every command a person or an agent types resolves to exactly one tier.

### Tier 1: Tilo's own commands are actions, and run anywhere

These need no Linux and no process:

- **Commands that change files:** `cp mv rm mkdir rmdir ln truncate touch
  append`, the environment's actions.
- **Commands that read:** `ls cat head tail wc stat find grep diff tree echo
  pwd cd test printf` and the like, run as reads over the environment's files,
  under the same grants.

They are written once, in Rust, against Tilo's file resources, and run in
whatever hosts the shell: the device, the workspace's host (tilo.coffee), or
the browser, compiled to WebAssembly. Writes are attributed and undoable
through the workspace's History.

### Tier 2: third-party programs run as WebAssembly, anywhere

WebAssembly is for existing third-party programs that aren't worth
rewriting, built for WASI. Tilo's own commands are tier 1. Examples:

- uutils coreutils;
- ripgrep and jq;
- a Python built for WASI;
- formatters and linters that build for wasm32-wasi.

On devices and hosts they run under wasmi. In the browser they run in the
browser's own WebAssembly engine, behind the browser runtime's isolation; no
WebAssembly interpreter is shipped to the browser.

A program's file system is the environment's granted folders, mapped onto
Tilo's mount. Every write goes through the mount, so it is versioned and
attributed, and one run is one action: one undo takes back everything the run
changed. There's no network unless the environment allows it, and then only
through Tilo's network rules. Tools are signed packages in the environment's
tool list (`pkg install` already adds tools there).

### Tier 3: Linux-only programs run in a container or VM, on demand

Compilers, native binaries, anything that needs syscalls WASI doesn't have,
and interactive Linux programs such as `bash`. Each command becomes a compute
job on the existing placement system:

- **Where it runs.** The relay places it where it can run: the person's own
  device, one of their relays, or a workspace host that offers compute.
  The sandbox is whatever that place runs: a bwrap guest on a Linux device, a
  microVM or a container on a host.
- **Inputs and outputs.** The inputs are the files the command can see, by
  digest. Its writes come back as file actions in the workspace, attributed,
  versioned, and grouped as one undoable run.
- **Lifetime.** The guest starts for the job and is kept while the session
  that asked for it is open, then stopped when the session closes. Its
  lifetime follows events (the session closes, the job ends), never a timer.
  A warm guest for an open Terminal session is that session's lease.
- **bash** is a tier-3 program like any other: `bash` in a session runs it
  in the session's guest, for as long as it runs.

### The shell itself

ush is a shell interpreter in Rust that runs anywhere: on a device, on a host,
in the browser. It parses the command line, expands it, and resolves each
command in turn: a function or built-in of the shell's own, a tier-1 command,
a tier-2 tool in the environment, then a tier-3 tool. Pipes connect tiers: in
`grep foo src | wc -l`, `grep` and `wc` are both tier 1, while in
`cargo build 2>&1 | grep error`, `cargo` is tier 3 and `grep` tier 1.

The language is POSIX sh, all of it, plus the bash features people use:
arrays, `[[ ]]`, `local`, `$'…'`, `${var//from/to}`, brace expansion and the
like. A feature it doesn't have is refused by name, never quietly skipped,
so a script that relied on it fails loudly. It builds without `fork` or
`exec`, so the same interpreter compiles to WebAssembly: a subshell is a copy
of the shell's state, and a pipe between two of its own commands is a pipe in
memory.

The run marks (OSC 133) and the run records stay as they are, so Terminal,
agents and History see one model whichever tier ran a command.

## How this plays out

- **On tilo.coffee, in the Lounge or a member's workspace:**
  - `ls`, `cat`, `mv`, `grep` and `jq` work right away, on the host or in the
    browser, with no VM;
  - `cargo build` asks for a compute place: the member's own device or relay
    if they have one, or the host's compute if the host offers it. If none is
    available, the shell says so in plain words.
- **On the person's own laptop:** tiers 1 and 2 run in-process, and tier 3 in
  the local guest, as today, but only for the commands that need it.
- **For agents:** an agent calls the same resolved commands, or the actions
  directly. Everything it changes is in History, attributed to it on behalf
  of its person, and undoable per run.

## What changes for existing environments

- An environment record keeps its tools and grants. Tools gain a tier, set
  by their package (WebAssembly or Linux image).
- The new shell is the shell of every Terminal environment session. bash
  is a program in the session's guest (tier 3), not a separate session.
- Until tier 3 runs as jobs, a command that isn't tier 1 says it needs tier 2
  or 3; in a guest on a Linux device, it runs in that guest, as it did before,
  so nothing a person relies on breaks.

## Order of work

1. The shell interpreter, running tier 1 (the read commands as reads, the
   file commands as the environment's actions), on the device, as the default
   for new Terminal sessions. Built: it runs on the Tilo side, outside every
   guest, its reads asked of the record's grants and every write (a redirect
   too, of any size, by reference to the environment's content store) an
   action of the record's, with `~` the workspace's folder, with a Linux guest started only for the
   first program a session runs, which then serves the session's programs
   one command at a time ([`ush.md`](ush.md), "The session's shell").
2. Tier 2: a WASI runner over Tilo's mount, with a few signed tool packages,
   on devices and hosts (wasmi) and in the browser (its own engine).
3. Tier 3 on the existing compute and placement system: one command as one
   job, writes as actions, guests scoped to the session, and a quota for
   compute on Tilo's own host.
4. The integration test: a workspace on a test site, with no Linux on the
   host, where `ls`, `jq` and `cargo build` all work, the last one on the
   member's device.

## Decisions

Decided on 2026-10-28:

1. **Shell language:** full POSIX sh, plus the bash features people use
   (arrays, `[[ ]]`, `$(...)`, and the like). Anything else is refused by
   name.
2. **Host compute:** hosts may offer tier-3 compute to members, but only in a
   limited way on Tilo's own host (a quota). Otherwise tier 3 runs on the
   person's own devices and relays.
3. **WebAssembly:**
   - only for existing third-party programs; everything Tilo's own is a Tilo
     action;
   - wasmi on devices and hosts;
   - in the browser, the browser's own engine, behind the browser runtime's
     isolation. wasmi is not shipped to the browser.
4. **Default:** the new ush is the default shell. A full Linux session (bash
   in a guest) stays available as tier 3. Since 2026-10-29 that is bash run
   as a program in the session's guest; the separate Linux session choice
   is gone.
