#!/usr/bin/env python3
"""The link stubs of the Apple frameworks, for a cross build of `tilo` for macOS on Linux.

    macos-frameworks.py DIR TARGET  write the stubs into DIR: DIR/<Framework>.framework/
                                    <Framework>.tbd, and DIR/lib<name>.tbd for the dylibs, of the
                                    crates TARGET's dependency graph reaches

A Mac build links its frameworks out of the SDK, which Apple licenses for Apple hardware only, and
which this builds without. What the linker needs of a framework is a text description of it: its
install name and the symbols it exports. Those are public -- the headers its functions are declared
in, and the `otool -L` output of any program that links it -- and this writes them out.

The symbols are those of the crates of the dependency graph, which is where the program's
references to them are. A crate links a framework (`#[link(name = "AppKit", kind = "framework")]`)
and declares in it the functions and classes it uses of that framework. Each such declaration is
what the linker must resolve, so every framework's stub lists all of them; a symbol no crate
refers to costs the stub a line.

The attribute is either on each of a crate's extern blocks, over that block's symbols, or once, on
an empty one: the generated bindings name their framework that way, and declare their classes
through a macro. The latter is the library the crate's other declarations are for.
"""

import collections
import json
import os
import re
import shutil
import subprocess
import sys

# The frameworks' install names, as a Mac's `otool -L` prints them: the file dyld loads for a
# reference to the framework.
INSTALL_NAMES = {
    "AVFoundation": "/System/Library/Frameworks/AVFoundation.framework/Versions/A/AVFoundation",
    "AppKit": "/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit",
    "ApplicationServices": "/System/Library/Frameworks/ApplicationServices.framework/Versions/A/ApplicationServices",
    "Carbon": "/System/Library/Frameworks/Carbon.framework/Versions/A/Carbon",
    "CoreFoundation": "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "CoreGraphics": "/System/Library/Frameworks/CoreGraphics.framework/Versions/A/CoreGraphics",
    "CoreLocation": "/System/Library/Frameworks/CoreLocation.framework/Versions/A/CoreLocation",
    "CoreMedia": "/System/Library/Frameworks/CoreMedia.framework/Versions/A/CoreMedia",
    "CoreServices": "/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices",
    "CoreSpotlight": "/System/Library/Frameworks/CoreSpotlight.framework/Versions/A/CoreSpotlight",
    "CoreVideo": "/System/Library/Frameworks/CoreVideo.framework/Versions/A/CoreVideo",
    "CoreWLAN": "/System/Library/Frameworks/CoreWLAN.framework/Versions/A/CoreWLAN",
    "Foundation": "/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation",
    "IOKit": "/System/Library/Frameworks/IOKit.framework/Versions/A/IOKit",
    "MediaPlayer": "/System/Library/Frameworks/MediaPlayer.framework/Versions/A/MediaPlayer",
    "Metal": "/System/Library/Frameworks/Metal.framework/Versions/A/Metal",
    "Network": "/System/Library/Frameworks/Network.framework/Versions/A/Network",
    "QuartzCore": "/System/Library/Frameworks/QuartzCore.framework/Versions/A/QuartzCore",
    "Security": "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    "SecurityFoundation": "/System/Library/Frameworks/SecurityFoundation.framework/Versions/A/SecurityFoundation",
    "ServiceManagement": "/System/Library/Frameworks/ServiceManagement.framework/Versions/A/ServiceManagement",
    "SystemConfiguration": "/System/Library/Frameworks/SystemConfiguration.framework/Versions/A/SystemConfiguration",
    "UniformTypeIdentifiers": "/System/Library/Frameworks/UniformTypeIdentifiers.framework/Versions/A/UniformTypeIdentifiers",
    "UserNotifications": "/System/Library/Frameworks/UserNotifications.framework/Versions/A/UserNotifications",
}

# The libraries the program links that are no frameworks: plain dylibs of the system, found by
# name in the stub directory.
DYLIBS = {
    "iconv": "/usr/lib/libiconv.2.dylib",
    "objc": "/usr/lib/libobjc.A.dylib",
}

TARGETS = "[ arm64-macos, x86_64-macos ]"

# A crate's link attribute: `#[link(name = "AppKit", kind = "framework")]`, or the default, a
# dylib, with no kind at all.
LINK = re.compile(r'link\(\s*name\s*=\s*"([^"]+)"\s*(?:,\s*kind\s*=\s*"(\w+)")?[^)]*\)')
# An extern block, and the macros a bindings crate wraps one in (`objc2`'s `extern_c!`).
EXTERN_BLOCK = re.compile(r'(?:extern\s+"(?:C|C-unwind|system)"|extern_c(?:_unwind)?!)\s*\{')
DECLARED = re.compile(r"\b(?:pub\s+)?(?:unsafe\s+)?(?:fn\s+([A-Za-z_]\w*)|static\s+([A-Za-z_]\w*))")
# A declaration's own symbol, when it is not its Rust name (`link_name`, and inside a
# `cfg_attr` the name some architectures use).
LINK_NAME = re.compile(r'link_name\s*=\s*"([^"]*)"')
EXTERN_CLASS = re.compile(r"extern_class!\(")
CLASS_NAME = re.compile(r'#\[unsafe\(name\s*=\s*"([^"]+)"\s*\)\]|pub\s+struct\s+([A-Za-z_]\w*)')
# Documentation shows attributes in examples (`//! #[link(name = "CoreGraphics", kind =
# "framework")]`) and prose names functions; only what the compiler sees counts.
COMMENT = re.compile(r"/\*[\s\S]*?\*/|^[ \t]*//[^\n]*$|[ \t]//[^\"\n]*$", re.MULTILINE)


def source_text(path):
    text = open(path, encoding="utf-8", errors="replace").read()
    return COMMENT.sub("", text)


def delimited(text, start, opening, closing):
    """The body of the first `opening`..`closing` region at or after `start`, nesting counted."""
    open_at = text.find(opening, start)
    if open_at < 0:
        return ""
    depth = 0
    for index in range(open_at, len(text)):
        if text[index] == opening:
            depth += 1
        elif text[index] == closing:
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : index]
    return text[open_at + 1 :]


def brace_block(text, start):
    """The body of the brace block whose `{` is at or after `start`, or nothing."""
    return delimited(text, start, "{", "}")


def symbol_name(raw):
    """A Rust declaration's name as the linker sees it: its link name when it has one."""
    return raw.replace("\\u{1}", "")


def extern_symbols(body):
    """The names an extern block's body declares, as the linker sees them.

    A declaration with a `link_name` is that name and no other: the Rust name is this program's,
    while the linker resolves the one it was given. Inside a `cfg_attr` it is one architecture's
    name of a declaration the others name with its Rust name, and no configuration is known here,
    so both are listed.
    """
    symbols = set()
    conditional = set()
    pending = None
    for line in body.splitlines():
        named = LINK_NAME.search(line)
        if named:
            if "cfg_attr" in line:
                conditional.add(symbol_name(named.group(1)))
            else:
                pending = symbol_name(named.group(1))
            continue
        declared = DECLARED.search(line)
        if declared:
            symbols.add(pending or declared.group(1) or declared.group(2))
            pending = None
    return symbols | conditional


def die(message):
    sys.exit(f"macos-frameworks.py: {message}")


def libsystem_symbols():
    """The symbols the system's C library exports, which no stub of this one lists.

    Zig ships a description of the C library (`libSystem.tbd`, whose documents are the libraries
    the C library reexports) and links everything on a Mac against it. A symbol it exports binds
    to the C library there too, never to a framework or to the Objective-C runtime, so a stub
    that listed it would point the reference at a library that does not have it.
    """
    zig = shutil.which("zig") or die("no zig to read the C library's symbols from")
    env = subprocess.run([zig, "env"], check=True, capture_output=True, text=True).stdout
    lib_dir = re.search(r'\.lib_dir = "([^"]*)"', env)
    if not lib_dir:
        die(f"{zig} env names no lib_dir")
    path = os.path.join(lib_dir.group(1), "libc/darwin/libSystem.tbd")
    if not os.path.isfile(path):
        die(f"no {path}, the C library's symbols")
    text = open(path, encoding="utf-8").read()
    return set(re.findall(r"\b_[A-Za-z_][A-Za-z0-9_$.]*", text))


def stub_symbol(name):
    return name if name.startswith("_OBJC_") else "_" + name


def known(name):
    """Whether a library a crate links is one this writes a stub of: a framework, or a dylib."""
    return name in INSTALL_NAMES or name in DYLIBS


def all_extern_symbols(text):
    """Every name the file declares in an extern block, attributed or not."""
    names = set()
    for match in EXTERN_BLOCK.finditer(text):
        names |= extern_symbols(brace_block(text, match.end() - 1))
    return names


def brace_span(text, start):
    """The body of the brace block whose `{` is at or after `start`, and where it ends."""
    open_at = text.find("{", start)
    if open_at < 0:
        return None, start
    body = delimited(text, open_at, "{", "}")
    return body, min(open_at + len(body) + 2, len(text))


def file_declarations(text):
    """What a file declares: (library, names) of its attributed blocks, else the library it names
    on an empty block.

    A generated bindings crate names its framework once, on `extern "C" {}`, and its functions and
    classes live in the files that block covers. A hand-written bindings crate puts the attribute
    on each of its blocks, over its own symbols.
    """
    attributed = collections.defaultdict(set)
    crate_level = []
    position = 0
    for match in LINK.finditer(text):
        if match.start() < position or match.group(2) not in (None, "dylib", "framework"):
            continue
        body, position = brace_span(text, match.end())
        if body is None:
            break
        names = extern_symbols(body)
        if names:
            attributed[match.group(1)] |= names
        elif match.group(1) not in crate_level:
            crate_level.append(match.group(1))
    return attributed, [name for name in crate_level if known(name)]


def class_symbols(text):
    """The Objective-C classes a file declares, which the Objective-C runtime looks up by name."""
    names = set()
    for match in EXTERN_CLASS.finditer(text):
        body = delimited(text, match.end() - 1, "(", ")")
        for class_match in CLASS_NAME.finditer(body):
            name = class_match.group(1) or class_match.group(2)
            if name:
                names |= {f"_OBJC_CLASS_$_{name}", f"_OBJC_METACLASS_$_{name}"}
    return names


def sources(directory):
    for root, dirs, files in os.walk(os.path.join(directory, "src")):
        dirs[:] = sorted(dirs)
        for name in sorted(files):
            if name.endswith(".rs"):
                yield os.path.join(root, name)


def graph(target):
    """The packages of one target's dependency graph: (name, directory) of each."""
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    packages = {package["id"]: package for package in metadata["packages"]}
    for node in metadata["resolve"]["nodes"]:
        package = packages[node["id"]]
        kinds = {kind for member in package["targets"] for kind in member["kind"]}
        if "proc-macro" in kinds:
            continue
        yield package["name"], os.path.dirname(package["manifest_path"])


def framework_symbols(target):
    """The symbols each framework's stub lists, read out of the crates of the target's graph."""
    symbols = collections.defaultdict(set)
    for name, directory in graph(target):
        files = []
        crate_level = []
        for path in sources(directory):
            text = source_text(path)
            attributed, file_level = file_declarations(text)
            files.append((text, attributed))
            for library in file_level:
                if library not in crate_level:
                    crate_level.append(library)
        # A crate that names more than one library -- the Objective-C runtime's bindings name the
        # runtime and the Foundation framework -- declares its own symbols for the first of them.
        if len(crate_level) > 1:
            print(
                f"macos-frameworks.py: {name} names {', '.join(crate_level)}; its declarations go "
                f"to {crate_level[0]}",
                file=sys.stderr,
            )
        for text, attributed in files:
            for library, names in attributed.items():
                symbols[library] |= {stub_symbol(name) for name in names}
            # What the file declares outside a block of its own belongs to the library its crate
            # names: generated bindings name theirs once, for every file they generate.
            declared = {stub_symbol(name) for name in all_extern_symbols(text)} | class_symbols(text)
            declared -= {stub_symbol(name) for names in attributed.values() for name in names}
            for library in crate_level[:1]:
                symbols[library] |= declared
    system = libsystem_symbols()
    named = sum(len(values & system) for library, values in symbols.items() if known(library))
    print(
        f"the C library's {len(system)} symbols, {named} of them named by a crate of the graph",
        file=sys.stderr,
    )
    symbols = {library: values - system for library, values in symbols.items()}
    return {library: values for library, values in symbols.items() if values}


def stub(install_name, symbols):
    listed = ",\n".join(
        "                       " + ", ".join(f"'{symbol}'" for symbol in row)
        for row in chunks(sorted(symbols), 6)
    )
    return (
        "--- !tapi-tbd\n"
        "tbd-version:     4\n"
        f"targets:         {TARGETS}\n"
        f"install-name:    '{install_name}'\n"
        "current-version: 1\n"
        "exports:\n"
        f"  - targets:     {TARGETS}\n"
        "    symbols:     [ >-\n"
        f"{listed}\n"
        "                 ]\n"
        "...\n"
    )


def chunks(items, size):
    for start in range(0, len(items), size):
        yield items[start : start + size]


def write_stubs(out, symbols):
    """Write a stub of each library that has symbols into DIR."""
    os.makedirs(out, exist_ok=True)
    for library in sorted(symbols):
        if not symbols[library] or not known(library):
            continue
        if library in INSTALL_NAMES:
            directory = os.path.join(out, f"{library}.framework")
            os.makedirs(directory, exist_ok=True)
            path = os.path.join(directory, f"{library}.tbd")
        else:
            path = os.path.join(out, f"lib{library}.tbd")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(stub(INSTALL_NAMES.get(library) or DYLIBS[library], symbols[library]))
        print(f"{library}: {len(symbols[library])} symbols", file=sys.stderr)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__.strip())
    write_stubs(sys.argv[1], framework_symbols(sys.argv[2]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
