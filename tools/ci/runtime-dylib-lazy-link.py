#!/usr/bin/env python3
"""Link `libruntime_dylib.so` so GLib and GStreamer load on first use.

Every thin app maps the shared runtime. gstreamer-rs is part of it (tilo-media
sits under tilo-ui), so a plain link lists libgstreamer, libglib, libgobject,
libgio and the GStreamer helper libraries as DT_NEEDED, and a calculator maps
and initialises a media framework before `main`.

`tools/ci/rustc-thin-app-wrapper.sh` makes this script rustc's linker for the
`runtime_dylib` crate only. It runs the real link (`TILO_LAZY_LINK_CC`, the
linker cargo chose, else `cc`) with these changes:

- the `-l` flags of the libraries matching `LAZY_LIBRARY` are dropped, so
  none of them becomes DT_NEEDED;
- for every function of theirs the link inputs reference, a generated object
  defines a hidden trampoline of the same name. It jumps through a slot in
  `.bss`; while the slot is empty it calls the loader, which `dlopen`s every
  dropped library (`RTLD_NOW | RTLD_GLOBAL`, by SONAME, from the same search
  path the loader would have used) and fills every slot with `dlsym`. The slow
  path saves every argument register, so variadic functions work. Slots need
  no load-time relocation, so a process that never plays media pays nothing;
- the one variable the runtime reads, `gst_memory_alignment`, is mirrored by a
  hidden copy refreshed after loading and after `gst_init`/`gst_init_check`
  (which the loader wraps); any other variable fails the link;
- the SONAMEs are recorded, NUL-separated, in the allocated section
  `.tilo.lazy_needed`. The runtime-bundle tools read it to ship and verify
  those libraries as they do DT_NEEDED ones.

The finished file is checked: no DT_NEEDED on a dropped library and no
undefined dynamic symbol one of them exports. A link with none of the
libraries on its line keeps them as they are.

Every link, lazy libraries or not, also sheds code no app can reach:

- rustc links a dylib without `--gc-sections`, so every function of every
  crate in it stays, used or not. The link collects sections: what nothing
  exported reaches goes. The crate metadata (`.rustc`) is not an allocated
  section, so it stays for the apps that compile against the runtime;
- rustc's export list names every public item of every crate in the dylib,
  so syn, quote and proc-macro2 (linked because zvariant_utils carries macro
  helpers) were exported and kept. Only proc macros use them, never an app;
  they leave the list, and with it the file.
"""

from __future__ import annotations

import concurrent.futures
import os
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field

LAZY_LIBRARY = re.compile(r"gst[a-z0-9]*-1\.0|g(?:lib|object|io|module|thread)-2\.0")
SYMBOL = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SECTION = ".tilo.lazy_needed"
GSTREAMER = "gstreamer-1.0"
# Variables the runtime may read: the loader keeps a hidden copy in step.
MIRRORED = {GSTREAMER: {"gst_memory_alignment"}}
# Functions the loader defines itself (to refresh the copies above).
WRAPPED = {GSTREAMER: {"gst_init", "gst_init_check"}}
INPUT_SUFFIXES = (".o", ".rlib", ".a")
GC = ["-Wl,--gc-sections"]
# The crates only proc macros use, by their v0-mangled crate roots, and the
# zvariant_utils module that links them in.
COMPILE_TIME_ONLY = re.compile(r"C(?:s[0-9A-Za-z]*_)?(?:3syn|11proc_macro2|5quote)|14zvariant_utils6macros")
# The crate a v0 symbol of a generic instance ends with: the crate that
# instantiated it (`...8grow_oneCs<hash>_11proc_macro2`). With shared
# generics that crate is where dependants link the instance from: rustc
# picks, among the upstream crates that export one, the one with the lowest
# crate id, which can be a compile-time-only crate. Such an instance belongs
# to `alloc` (or wherever its path starts), not to that crate.
CRATE_ROOT = re.compile(r"C(?:s[0-9A-Za-z]*_)?([0-9]+)")
NM_BATCH = 64
PROGRAM = "runtime-dylib-lazy-link"


class LinkError(Exception):
    pass


@dataclass
class Library:
    name: str
    path: pathlib.Path
    soname: str
    functions: set[str] = field(default_factory=set)
    variables: set[str] = field(default_factory=set)


def parse_response(text: str) -> list[str]:
    """Arguments of a GCC response file: whitespace-separated, quotes and backslashes escape."""
    arguments: list[str] = []
    current: list[str] = []
    pending = False
    quote = ""
    characters = iter(text)
    for character in characters:
        if character == "\\":
            current.append(next(characters, ""))
            pending = True
        elif quote:
            if character == quote:
                quote = ""
            else:
                current.append(character)
        elif character in "'\"":
            quote = character
            pending = True
        elif character.isspace():
            if pending or current:
                arguments.append("".join(current))
            current, pending = [], False
        else:
            current.append(character)
            pending = True
    if pending or current:
        arguments.append("".join(current))
    return arguments


def format_response(arguments: list[str]) -> str:
    """One argument per line, escaped the way `parse_response` (and GCC) read it."""
    return "".join((re.sub(r"([\\\s'\"])", r"\\\1", argument) or '""') + "\n" for argument in arguments)


def expand(argv: list[str]) -> tuple[list[str], bool]:
    """The link line with response files read in, and whether one was used."""
    arguments: list[str] = []
    used = False
    for argument in argv:
        if argument.startswith("@") and pathlib.Path(argument[1:]).is_file():
            arguments += parse_response(pathlib.Path(argument[1:]).read_text(encoding="utf-8"))
            used = True
        else:
            arguments.append(argument)
    return arguments, used


def split_libraries(arguments: list[str]) -> tuple[list[str], list[str]]:
    """The link line without the lazily loaded libraries, and their names in link order."""
    kept: list[str] = []
    lazy: list[str] = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        name = None
        if argument == "-l" and index + 1 < len(arguments):
            name = arguments[index + 1]
        elif argument.startswith("-l"):
            name = argument[2:]
        if name is not None and LAZY_LIBRARY.fullmatch(name):
            if name not in lazy:
                lazy.append(name)
            index += 2 if argument == "-l" else 1
            continue
        kept.append(argument)
        index += 1
    return kept, lazy


def search_directories(arguments: list[str]) -> list[pathlib.Path]:
    directories = []
    for index, argument in enumerate(arguments):
        if argument == "-L" and index + 1 < len(arguments):
            directories.append(pathlib.Path(arguments[index + 1]))
        elif argument.startswith("-L") and len(argument) > 2:
            directories.append(pathlib.Path(argument[2:]))
    return directories


def output_path(arguments: list[str]) -> pathlib.Path:
    for index, argument in enumerate(arguments):
        if argument == "-o" and index + 1 < len(arguments):
            return pathlib.Path(arguments[index + 1])
    raise LinkError("the link line names no output (-o)")


def link_inputs(arguments: list[str]) -> list[pathlib.Path]:
    """The object files and archives on the link line, whose references the stubs serve."""
    return [pathlib.Path(argument) for argument in arguments
            if not argument.startswith("-") and argument.endswith(INPUT_SUFFIXES)
            and pathlib.Path(argument).is_file()]


def run(command: list[str], check: bool = True) -> str:
    result = subprocess.run(command, capture_output=True, text=True)
    if check and result.returncode != 0:
        raise LinkError(f"{shlex.join(command)} failed:\n{result.stderr.strip()}")
    return result.stdout


def resolve(name: str, directories: list[pathlib.Path], compiler: str) -> pathlib.Path:
    """The shared object `-l<name>` would have linked."""
    filename = f"lib{name}.so"
    for directory in directories:
        candidate = directory / filename
        if candidate.is_file():
            return candidate.resolve()
    printed = run([compiler, f"-print-file-name={filename}"]).strip()
    candidate = pathlib.Path(printed)
    if candidate.is_absolute() and candidate.is_file():
        return candidate.resolve()
    raise LinkError(f"cannot find {filename} in {[str(d) for d in directories]} or {compiler}'s search path")


def dynamic_section(path: pathlib.Path) -> tuple[str | None, list[str]]:
    """SONAME and DT_NEEDED of a shared object."""
    soname = None
    needed = []
    for line in run(["readelf", "-dW", str(path)]).splitlines():
        match = re.search(r"\((SONAME|NEEDED)\)\s+\S.*\[([^\]]+)\]", line)
        if match and match.group(1) == "SONAME":
            soname = match.group(2)
        elif match:
            needed.append(match.group(2))
    return soname, needed


def dynamic_symbols(path: pathlib.Path) -> tuple[set[str], set[str], set[str]]:
    """Defined functions, defined variables and undefined names of a shared object's .dynsym."""
    functions: set[str] = set()
    variables: set[str] = set()
    undefined: set[str] = set()
    for line in run(["readelf", "-W", "--dyn-syms", str(path)]).splitlines():
        fields = line.split()
        if len(fields) < 8 or not fields[0].endswith(":") or not fields[0][:-1].isdigit():
            continue
        kind, binding, visibility, section, name = fields[3], fields[4], fields[5], fields[6], fields[7]
        name = name.split("@", 1)[0]
        if not name or binding not in ("GLOBAL", "WEAK"):
            continue
        if section == "UND":
            undefined.add(name)
        elif visibility in ("DEFAULT", "PROTECTED"):
            if kind in ("FUNC", "IFUNC", "GNU_IFUNC"):
                functions.add(name)
            elif kind != "NOTYPE":
                variables.add(name)
    return functions, variables, undefined


def library(name: str, directories: list[pathlib.Path], compiler: str) -> Library:
    path = resolve(name, directories, compiler)
    soname, _ = dynamic_section(path)
    if not soname:
        raise LinkError(f"{path} has no SONAME to load it by")
    functions, variables, _ = dynamic_symbols(path)
    return Library(name, path, soname, functions, variables)


def undefined_references(inputs: list[pathlib.Path]) -> set[str]:
    """Every symbol the link inputs leave undefined (a superset of what the link uses)."""
    batches = [inputs[start:start + NM_BATCH] for start in range(0, len(inputs), NM_BATCH)]

    def scan(batch: list[pathlib.Path]) -> set[str]:
        # An rlib's metadata member is not an object, so nm complains about it
        # and exits non-zero; the members it can read are still listed.
        listing = run(["nm", "--undefined-only", "--format=posix", "--no-sort", *map(str, batch)],
                      check=False)
        names = set()
        for line in listing.splitlines():
            fields = line.split()
            if len(fields) >= 2 and fields[1] in ("U", "w", "v"):
                names.add(fields[0].split("@", 1)[0])
        return names

    workers = max(1, min(8, os.cpu_count() or 1))
    with concurrent.futures.ThreadPoolExecutor(workers) as pool:
        return set().union(*pool.map(scan, batches)) if batches else set()


def plan(libraries: list[Library], referenced: set[str]) -> tuple[list[tuple[str, int]], list[str]]:
    """(symbol, library index) for each trampoline, and the references no trampoline can serve."""
    owner: dict[str, tuple[int, bool]] = {}
    for index, lib in enumerate(libraries):
        for name in lib.functions:
            owner.setdefault(name, (index, True))
        for name in lib.variables:
            owner.setdefault(name, (index, False))
    stubs: list[tuple[str, int]] = []
    problems: list[str] = []
    for name in sorted(referenced):
        if name not in owner:
            continue
        index, function = owner[name]
        lib = libraries[index]
        if not function:
            if name not in MIRRORED.get(lib.name, ()):
                problems.append(f"the runtime reads {lib.soname}'s variable {name}, which cannot exist "
                                "before the library loads; mirror it in the loader or stop reading it")
        elif name in WRAPPED.get(lib.name, ()):
            continue
        elif not SYMBOL.fullmatch(name):
            problems.append(f"{lib.soname} function {name!r} is not an assembler identifier")
        else:
            stubs.append((name, index))
    return stubs, problems


def architecture(compiler: str) -> str:
    machine = run([compiler, "-dumpmachine"]).strip()
    for arch in ("x86_64", "aarch64"):
        if machine.startswith(arch):
            return arch
    raise LinkError(f"no lazy trampolines for {machine}")


X86_64_LOAD = """\
\t.text
\t.p2align 4
\t.type __tilo_lazy_load, @function
# Called from a trampoline whose slot is still empty: keep every argument
# register (rax carries a variadic call's vector count) across the resolver.
__tilo_lazy_load:
\tpushq %rbp
\tmovq %rsp, %rbp
\tpushq %rdi
\tpushq %rsi
\tpushq %rdx
\tpushq %rcx
\tpushq %r8
\tpushq %r9
\tpushq %rax
\tpushq %r10
\tsubq $136, %rsp
""" + "".join(f"\tmovdqu %xmm{i}, {16 * i}(%rsp)\n" for i in range(8)) + """\
\tcall __tilo_lazy_resolve
""" + "".join(f"\tmovdqu {16 * i}(%rsp), %xmm{i}\n" for i in range(8)) + """\
\taddq $136, %rsp
\tpopq %r10
\tpopq %rax
\tpopq %r9
\tpopq %r8
\tpopq %rcx
\tpopq %rdx
\tpopq %rsi
\tpopq %rdi
\tpopq %rbp
\tret
\t.size __tilo_lazy_load, .-__tilo_lazy_load
"""

AARCH64_LOAD = """\
\t.text
\t.p2align 2
\t.type __tilo_lazy_load, %function
// Called from a trampoline whose slot is still empty: keep every argument
// register (x8 is the indirect result) across the resolver.
__tilo_lazy_load:
\tstp x29, x30, [sp, #-224]!
\tmov x29, sp
\tstp x0, x1, [sp, #16]
\tstp x2, x3, [sp, #32]
\tstp x4, x5, [sp, #48]
\tstp x6, x7, [sp, #64]
\tstp x8, x9, [sp, #80]
\tstp q0, q1, [sp, #96]
\tstp q2, q3, [sp, #128]
\tstp q4, q5, [sp, #160]
\tstp q6, q7, [sp, #192]
\tbl __tilo_lazy_resolve
\tldp q6, q7, [sp, #192]
\tldp q4, q5, [sp, #160]
\tldp q2, q3, [sp, #128]
\tldp q0, q1, [sp, #96]
\tldp x8, x9, [sp, #80]
\tldp x6, x7, [sp, #64]
\tldp x4, x5, [sp, #48]
\tldp x2, x3, [sp, #32]
\tldp x0, x1, [sp, #16]
\tldp x29, x30, [sp], #224
\tret
\t.size __tilo_lazy_load, .-__tilo_lazy_load
"""


def trampoline(arch: str, name: str, index: int) -> str:
    slot = f".Ltilo_slot{index}"
    head = f"\t.globl {name}\n\t.hidden {name}\n"
    if arch == "x86_64":
        return (f"\t.p2align 4\n{head}\t.type {name}, @function\n{name}:\n"
                f"\tcmpq $0, {slot}(%rip)\n\tje .Ltilo_load{index}\n\tjmp *{slot}(%rip)\n"
                f".Ltilo_load{index}:\n\tcall __tilo_lazy_load\n\tjmp *{slot}(%rip)\n"
                f"\t.size {name}, .-{name}\n")
    load = f"\tadrp x16, {slot}\n\tadd x16, x16, :lo12:{slot}\n\tldar x17, [x16]\n"
    return (f"\t.p2align 2\n{head}\t.type {name}, %function\n{name}:\n{load}"
            f"\tcbz x17, .Ltilo_load{index}\n\tbr x17\n"
            f".Ltilo_load{index}:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n"
            f"\tbl __tilo_lazy_load\n\tldp x29, x30, [sp], #16\n{load}\tbr x17\n"
            f"\t.size {name}, .-{name}\n")


def assembly(arch: str, libraries: list[Library], stubs: list[tuple[str, int]]) -> str:
    """Trampolines, their slots, the symbol table the loader walks and `.tilo.lazy_needed`."""
    progbits = "@progbits" if arch == "x86_64" else "%progbits"
    data = "@object" if arch == "x86_64" else "%object"
    comment = "#" if arch == "x86_64" else "//"
    parts = [f"{comment} Generated by tools/ci/{PROGRAM}.py.\n",
             X86_64_LOAD if arch == "x86_64" else AARCH64_LOAD, "\t.text\n"]
    parts += [trampoline(arch, name, index) for index, (name, _) in enumerate(stubs)]
    parts.append(f"\t.section {SECTION},\"a\",{progbits}\n\t.globl __tilo_lazy_libraries\n"
                 f"\t.hidden __tilo_lazy_libraries\n\t.type __tilo_lazy_libraries, {data}\n"
                 "__tilo_lazy_libraries:\n")
    parts += [f"\t.asciz \"{lib.soname}\"\n" for lib in libraries]
    parts.append("\t.byte 0\n")
    # Offsets, not pointers: the table needs no relocation at load.
    parts.append(f"\t.section .rodata\n\t.p2align 2\n\t.globl __tilo_lazy_entries\n"
                 f"\t.hidden __tilo_lazy_entries\n\t.type __tilo_lazy_entries, {data}\n"
                 "__tilo_lazy_entries:\n")
    for index, (_, owner) in enumerate(stubs):
        parts.append(f"\t.long .Ltilo_name{index} - .\n\t.long .Ltilo_slot{index} - .\n"
                     f"\t.long {owner}\n\t.long 0\n")
    parts.append(f"\t.globl __tilo_lazy_entry_count\n\t.hidden __tilo_lazy_entry_count\n"
                 f"\t.type __tilo_lazy_entry_count, {data}\n"
                 f"__tilo_lazy_entry_count:\n\t.long {len(stubs)}\n")
    parts += [f".Ltilo_name{index}:\n\t.asciz \"{name}\"\n" for index, (name, _) in enumerate(stubs)]
    parts.append("\t.bss\n\t.p2align 3\n")
    parts += [f".Ltilo_slot{index}:\n\t.zero 8\n" for index in range(len(stubs))]
    parts.append(f"\t.section .note.GNU-stack,\"\",{progbits}\n")
    return "".join(parts)


LOADER = r"""/* Generated by tools/ci/runtime-dylib-lazy-link.py: the loader
 * behind the lazy GLib/GStreamer trampolines in libruntime_dylib.so. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define HIDDEN __attribute__((visibility("hidden")))
#define MAX_LIBRARIES 32

struct tilo_lazy_entry {
    int32_t name;
    int32_t slot;
    uint32_t library;
    uint32_t reserved;
};

extern const char __tilo_lazy_libraries[] HIDDEN;
extern const struct tilo_lazy_entry __tilo_lazy_entries[] HIDDEN;
extern const uint32_t __tilo_lazy_entry_count HIDDEN;

static void *handles[MAX_LIBRARIES];
static pthread_once_t once = PTHREAD_ONCE_INIT;

/* A C function has no way to report that its library is missing; failing here
 * is what the loader did at exec while the library was DT_NEEDED. */
__attribute__((noreturn)) static void fail(const char *what, const char *name) {
    const char *why = dlerror();
    fprintf(stderr, "tilo runtime: cannot %s %s for media: %s\n", what, name,
            why ? why : "not found");
    abort();
}

static const void *field(const int32_t *offset) {
    return (const char *)offset + *offset;
}

#ifdef TILO_LAZY_GSTREAMER
/* gst-rs reads this variable directly (gst::Memory::default_alignment). Until
 * GStreamer is loaded it holds GStreamer's own default (MEMORY_ALIGNMENT_MALLOC);
 * afterwards it follows the library, which may change it in gst_init. */
HIDDEN size_t gst_memory_alignment = 7;

static void mirror_gstreamer(void) {
    const size_t *real = dlsym(handles[TILO_LAZY_GSTREAMER], "gst_memory_alignment");
    if (real)
        __atomic_store_n(&gst_memory_alignment, *real, __ATOMIC_RELAXED);
}
#endif

static void load(void) {
    unsigned count = 0;
    for (const char *name = __tilo_lazy_libraries; *name; name += strlen(name) + 1) {
        if (count == MAX_LIBRARIES) {
            fprintf(stderr, "tilo runtime: more than %d lazy libraries\n", MAX_LIBRARIES);
            abort();
        }
        handles[count] = dlopen(name, RTLD_NOW | RTLD_GLOBAL);
        if (!handles[count])
            fail("load", name);
        count++;
    }
    for (uint32_t index = 0; index < __tilo_lazy_entry_count; index++) {
        const struct tilo_lazy_entry *entry = &__tilo_lazy_entries[index];
        const char *symbol = field(&entry->name);
        void **slot = (void **)field(&entry->slot);
        void *address = entry->library < count ? dlsym(handles[entry->library], symbol) : NULL;
        if (!address)
            fail("resolve", symbol);
        __atomic_store_n(slot, address, __ATOMIC_RELEASE);
    }
#ifdef TILO_LAZY_GSTREAMER
    mirror_gstreamer();
#endif
}

HIDDEN void __tilo_lazy_resolve(void) {
    pthread_once(&once, load);
}

#ifdef TILO_LAZY_GSTREAMER
static void *gstreamer_function(const char *name) {
    __tilo_lazy_resolve();
    void *address = dlsym(handles[TILO_LAZY_GSTREAMER], name);
    if (!address)
        fail("resolve", name);
    return address;
}

HIDDEN int gst_init_check(int *argc, char ***argv, void **error) {
    int (*real)(int *, char ***, void **) =
        (int (*)(int *, char ***, void **))gstreamer_function("gst_init_check");
    int initialised = real(argc, argv, error);
    mirror_gstreamer();
    return initialised;
}

HIDDEN void gst_init(int *argc, char ***argv) {
    void (*real)(int *, char ***) = (void (*)(int *, char ***))gstreamer_function("gst_init");
    real(argc, argv);
    mirror_gstreamer();
}
#endif
"""


def check_output(path: pathlib.Path, libraries: list[Library]) -> list[str]:
    """What would make the linked runtime load a lazy library at exec, or fail to."""
    _, needed = dynamic_section(path)
    problems = [f"{path.name} still names {name} as DT_NEEDED"
                for name in needed if name in {lib.soname for lib in libraries}]
    _, _, undefined = dynamic_symbols(path)
    exported = set().union(*(lib.functions | lib.variables for lib in libraries))
    left = sorted(undefined & exported)
    if left:
        problems.append(f"{path.name} leaves {len(left)} symbols of the lazy libraries undefined, "
                        f"which the loader would look up at exec: {' '.join(left[:20])}")
    return problems


def compile_objects(compiler: str, arch: str, libraries: list[Library], stubs: list[tuple[str, int]],
                    directory: pathlib.Path) -> list[str]:
    source = directory / "tilo-lazy-trampolines.s"
    source.write_text(assembly(arch, libraries, stubs), encoding="utf-8")
    loader = directory / "tilo-lazy-loader.c"
    loader.write_text(LOADER, encoding="utf-8")
    flags = ["-O2", "-fPIC", "-fvisibility=hidden", "-fno-strict-aliasing", "-fstack-protector-strong",
             "-std=gnu11", "-g0"]
    gstreamer = [index for index, lib in enumerate(libraries) if lib.name == GSTREAMER]
    if gstreamer:
        flags.append(f"-DTILO_LAZY_GSTREAMER={gstreamer[0]}")
    objects = [directory / "tilo-lazy-trampolines.o", directory / "tilo-lazy-loader.o"]
    run([compiler, "-c", str(source), "-o", str(objects[0])])
    run([compiler, *flags, "-c", str(loader), "-o", str(objects[1])])
    return [str(path) for path in objects]


def compile_time_only(line: str) -> bool:
    """Whether a version-script line exports an item of a compile-time-only crate.

    A generic instance that only names such a crate as the crate that
    instantiated it is someone else's item: dependants are compiled to link
    it from this library, so it stays exported.
    """
    symbol = line.strip().removesuffix(";")
    for root in CRATE_ROOT.finditer(symbol):
        # A crate root whose name runs exactly to the end of the symbol.
        if len(symbol) - root.end() == int(root.group(1)):
            symbol = symbol[: root.start()]
            break
    return bool(COMPILE_TIME_ONLY.search(symbol))


def narrow_exports(arguments: list[str]) -> int:
    """Leave the compile-time-only crates out of rustc's export list; how many were left out."""
    removed = 0
    for argument in arguments:
        for option in argument.split(","):
            if not option.startswith("--version-script="):
                continue
            script = pathlib.Path(option.removeprefix("--version-script="))
            lines = script.read_text(encoding="utf-8").splitlines(keepends=True)
            kept = [line for line in lines if not compile_time_only(line)]
            removed += len(lines) - len(kept)
            script.write_text("".join(kept), encoding="utf-8")
    return removed


def link(linker: str, arguments: list[str], response: bool, directory: pathlib.Path) -> int:
    if response:
        file = directory / "linker-arguments"
        file.write_text(format_response(arguments), encoding="utf-8")
        command = [linker, f"@{file}"]
    else:
        command = [linker, *arguments]
    return subprocess.run(command).returncode


def main(argv: list[str]) -> int:
    linker = os.environ.get("TILO_LAZY_LINK_CC") or "cc"
    compiler = os.environ.get("TILO_LAZY_LINK_COMPILER") or "cc"
    arguments, response = expand(argv)
    hidden = narrow_exports(arguments)
    if os.environ.get("TILO_LAZY_LINK_VERBOSE"):
        print(f"{PROGRAM}: {hidden} compile-time-only symbols not exported", file=sys.stderr)
    kept, names = split_libraries(arguments)
    if not names:
        return subprocess.run([linker, *argv, *GC]).returncode
    try:
        directories = search_directories(arguments)
        libraries = [library(name, directories, compiler) for name in names]
        stubs, problems = plan(libraries, undefined_references(link_inputs(arguments)))
        if problems:
            raise LinkError("\n".join(problems))
        output = output_path(arguments)
        with tempfile.TemporaryDirectory(prefix="tilo-lazy-link-") as scratch:
            directory = pathlib.Path(scratch)
            objects = compile_objects(compiler, architecture(compiler), libraries, stubs, directory)
            status = link(linker, kept + objects + GC, response, directory)
        if status != 0:
            return status
        problems = check_output(output, libraries)
        if problems:
            output.unlink(missing_ok=True)
            raise LinkError("\n".join(problems))
    except (LinkError, OSError) as error:
        print(f"{PROGRAM}: {error}", file=sys.stderr)
        return 1
    if os.environ.get("TILO_LAZY_LINK_VERBOSE"):
        print(f"{PROGRAM}: {output.name}: {len(stubs)} functions of "
              f"{' '.join(lib.soname for lib in libraries)} load on first use", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
