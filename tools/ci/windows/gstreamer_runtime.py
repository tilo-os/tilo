"""Stage the GStreamer tilolang-process plays video with on Windows, from GStreamer's own MSVC build.

    gstreamer_runtime.py stage <out>   fetch the pinned release, stage the reviewed closure

GStreamer publishes its MSVC build as Python wheels on PyPI (cerbero's packages/wheel): the same
binaries its Windows installer carries, as zip archives a Linux build reads without running an
installer. The wheels are fetched at the digests pinned here, and the closure is taken out of them:

    <out>/gstreamer-runtime-x86_64.tar.zst    bin/, lib/gstreamer-1.0/, libexec/ and licenses/,
                                              the tree that goes beside bin/tilolang-process.exe
    <out>/gstreamer-runtime-x86_64.json       every staged file's digest, licence and notice
    <out>/gstreamer-sdk-x86_64.tar.zst        import libraries and pkg-config files for the
                                              libraries gstreamer-rs links, for a cross-build

each with a SHA-256 sidecar.

The closure is a player: nothing under the GPL and no encoder library of its own. H.264, HEVC, VP9
and AV1 decode through Direct3D 11's video decoders, which the GPU driver provides, and AAC and MP3
through Media Foundation's decoders; AV1, VP8 and VP9, Opus, Vorbis and FLAC also decode through
their BSD reference decoders (libvpx is linked into its plugin with its encoder, and libFLAC and
libopus carry theirs in the same library, as on a Mac). Vorbis decodes through lewton, a decoder
alone, because GStreamer's vorbis plugin loads libvorbisenc; Theora is not played, because its
plugin loads libtheoraenc. It is the set dist/macos/collection_media_runtime.py ships on a Mac,
with Windows' audio output and system decoders in place of Core Audio's and VideoToolbox's.

Every file is reviewed below with its licence and the notice that ships with it: a closure with
anything more or less, a GPL licence or an encoder library is refused, so a new GStreamer release
is reviewed before it ships.
"""
import hashlib
import io
import json
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

VERSION = '1.28.9'
WHEELS = {
    'gstreamer_libs': (
        'https://files.pythonhosted.org/packages/02/59/e1e66a76286e538cb250a7a0bd57656933f5844d8ee77519b67102dcabcf/gstreamer_libs-1.28.9-cp39-abi3-win_amd64.whl',
        '624c342cea158a877832fc238adc3223327aaf0f36582f4e3007229de4308157'),
    'gstreamer_plugins': (
        'https://files.pythonhosted.org/packages/12/c5/06069d55e4a1c7382db21976485e818e741d8003579f7b806f6eb193796b/gstreamer_plugins-1.28.9-cp39-abi3-win_amd64.whl',
        'b2f66ed28b6b45f5390cbe026474dfd7bf152c81218d038e59cff34e95ec5897'),
    'gstreamer_plugins_libs': (
        'https://files.pythonhosted.org/packages/51/ce/6e7b0fac572ac563bc99bbd0345c69f470ea6c43ff9ff0bc512e977e048e/gstreamer_plugins_libs-1.28.9-cp39-abi3-win_amd64.whl',
        '28d92c207de093461d1711ea2c5f2ca1f66bd62b79017e335f7d751fa1965629'),
    # The MSVC runtime the build links against, which not every Windows has.
    'gstreamer_ext_runtime': (
        'https://files.pythonhosted.org/packages/25/e9/1b3d79c7205788cd064c99762f4649ffb57c413c5eb0d48163c1b765ce7a/gstreamer_ext_runtime-1.28.9-cp39-abi3-win_amd64.whl',
        'dd9994cd35777c02a740a09063094021772ac0d2386998d722e65c09e8e0aafd'),
}
LGPL = 'LGPL-2.1-or-later'
# The plugins and the licence each declares.
PLUGINS = {
    # Playback itself.
    **dict.fromkeys(['coreelements', 'app', 'playback', 'typefindfunctions', 'videoconvertscale',
                     'videofilter', 'deinterlace', 'audioconvert', 'audioresample', 'autodetect',
                     'volume', 'audioparsers', 'videoparsersbad', 'isomp4', 'matroska',
                     'subparse'], LGPL),
    # Windows' audio output, the system's decoders, and the reference decoders.
    **dict.fromkeys(['wasapi2', 'd3d11', 'mediafoundation', 'dav1d', 'vpx', 'opus', 'flac', 'ogg'],
                    LGPL),
    'lewton': 'MIT',
    # What Music needs beside a player (collection_media_runtime.py TILOLANG_MUSIC, less asf,
    # which this build does not carry) and the network protocols its catalogue reaches.
    **dict.fromkeys(['audiofx', 'wavparse', 'aiff', 'id3demux', 'apetag'], LGPL),
    'reqwest': 'MPL-2.0',
}
# Every DLL the plugins and the scanner load, with its licence and the notices it ships with
# (tools/windows/gstreamer-licenses, at the versions GStreamer 1.28's cerbero recipes build).
GLIB = (LGPL, ['LGPL-2.1-or-later.txt'])
GST = (LGPL, ['LGPL-2.1-or-later.txt'])
MSVC = ('LicenseRef-MSVC-Redistributable', [])
LIBRARIES = {
    'glib-2.0-0.dll': GLIB, 'gobject-2.0-0.dll': GLIB, 'gmodule-2.0-0.dll': GLIB,
    'gio-2.0-0.dll': GLIB, 'intl-8.dll': ('LGPL-2.0-or-later', ['LGPL-2.0-or-later.txt']),
    'ffi-7.dll': ('MIT', ['libffi-3.2.9999.5-LICENSE']),
    'pcre2-8-0.dll': ('BSD-3-Clause', ['pcre2-10.42-LICENCE']),
    'z-1.dll': ('Zlib', ['zlib-1.3.1-LICENSE']),
    'bz2.dll': ('bzip2-1.0.6', ['bzip2-1.0.8-LICENSE']),
    'orc-0.4-0.dll': ('BSD-3-Clause', ['orc-0.4.42-COPYING']),
    'ogg-0.dll': ('BSD-3-Clause', ['libogg-1.3.5-COPYING']),
    'opus-0.dll': ('BSD-3-Clause', ['opus-1.5.2-COPYING']),
    'FLAC-8.dll': ('BSD-3-Clause', ['flac-1.4.3-COPYING.Xiph']),
    'dav1d.dll': ('BSD-2-Clause', ['dav1d-1.4.1-COPYING']),
    **{name: GST for name in [
        'gstreamer-1.0-0.dll', 'gstbase-1.0-0.dll', 'gstapp-1.0-0.dll', 'gstaudio-1.0-0.dll',
        'gstvideo-1.0-0.dll', 'gstpbutils-1.0-0.dll', 'gsttag-1.0-0.dll', 'gstriff-1.0-0.dll',
        'gstrtp-1.0-0.dll', 'gstfft-1.0-0.dll', 'gstcodecparsers-1.0-0.dll', 'gstcodecs-1.0-0.dll',
        'gstd3d11-1.0-0.dll', 'gstd3dshader-1.0-0.dll', 'gstdxva-1.0-0.dll', 'gstwinrt-1.0-0.dll']},
    'msvcp140.dll': MSVC, 'vcruntime140.dll': MSVC, 'vcruntime140_1.dll': MSVC,
}
# What the plugins link in rather than load, and the notices that come with them.
LINKED = {
    'vpx': ['libvpx-1.14.1-LICENSE', 'libvpx-1.14.1-PATENTS'],
    'lewton': ['lewton-0.10.2-LICENSE', 'gstreamer-rs-0.24-LICENSE-MIT', 'gtk-rs-core-0.21-LICENSE'],
    # The plugin is under the MPL; reqwest, hyper and rustls under Apache 2.0 (or MIT), ring
    # under Apache 2.0 and ISC.
    'reqwest': ['MPL-2.0.txt', 'Apache-2.0.txt', 'ISC.txt', 'gstreamer-rs-0.24-LICENSE-MIT',
                'gtk-rs-core-0.21-LICENSE'],
}
# What gstreamer-rs and glib-rs link, by their pkg-config names: an import library generated
# from each DLL's exports, and a pkg-config file naming it at the version the DLL is.
GLIB_VERSION = '2.82.4'
PKGCONFIG = {
    'glib-2.0': ('glib-2.0-0.dll', GLIB_VERSION),
    'gobject-2.0': ('gobject-2.0-0.dll', GLIB_VERSION),
    'gmodule-2.0': ('gmodule-2.0-0.dll', GLIB_VERSION),
    'gio-2.0': ('gio-2.0-0.dll', GLIB_VERSION),
    'gstreamer-1.0': ('gstreamer-1.0-0.dll', VERSION),
    'gstreamer-base-1.0': ('gstbase-1.0-0.dll', VERSION),
    'gstreamer-app-1.0': ('gstapp-1.0-0.dll', VERSION),
    'gstreamer-video-1.0': ('gstvideo-1.0-0.dll', VERSION),
    'gstreamer-audio-1.0': ('gstaudio-1.0-0.dll', VERSION),
    'gstreamer-pbutils-1.0': ('gstpbutils-1.0-0.dll', VERSION),
    'gstreamer-tag-1.0': ('gsttag-1.0-0.dll', VERSION),
}
NOTICES = Path(__file__).resolve().parents[3] / 'tools/windows/gstreamer-licenses'
SCANNER = 'libexec/gstreamer-1.0/gst-plugin-scanner.exe'
# Whatever the lists above say, these never ship: GPL code and encoder libraries. The lists
# already keep them out; this says why, so a review that adds one is refused in words.
REFUSED_LICENCE = re.compile(r'(^|[^L])GPL', re.IGNORECASE)
ENCODERS = re.compile(
    r'x264|x265|enc[-.]|enc$|avcodec|avformat|avfilter|swscale|swresample|libav|lame|faac|fdk'
    r'|openh264|svtav1|rav1e|aom|voaac|voamr|mpeg2|a52|dvdread|dvdnav', re.IGNORECASE)
AMD64 = 0x8664


def digest(data):
    return hashlib.sha256(data).hexdigest()


class Executable:
    """A Windows executable's headers: its machine, and the tables its data directories point at."""

    def __init__(self, data):
        if data[:2] != b'MZ':
            raise ValueError('not a Windows executable')
        header = struct.unpack_from('<I', data, 0x3c)[0]
        if data[header:header + 4] != b'PE\0\0':
            raise ValueError('not a Windows executable')
        self.data = data
        self.machine, sections = struct.unpack_from('<HH', data, header + 4)
        optional_size = struct.unpack_from('<H', data, header + 20)[0]
        optional = header + 24
        plus = struct.unpack_from('<H', data, optional)[0] == 0x20b
        self.directories = optional + (112 if plus else 96)
        self.sections = []
        for index in range(sections):
            entry = optional + optional_size + 40 * index
            virtual_size, address, raw_size, raw = struct.unpack_from('<IIII', data, entry + 8)
            characteristics = struct.unpack_from('<I', data, entry + 36)[0]
            self.sections.append((address, max(virtual_size, raw_size), raw,
                                  bool(characteristics & 0x20000000)))

    def offset(self, rva):
        for address, size, raw, _ in self.sections:
            if address <= rva < address + size:
                return rva - address + raw
        raise ValueError(f'address {rva:#x} is in no section')

    def code(self, rva):
        return any(address <= rva < address + size and executable
                   for address, size, _, executable in self.sections)

    def string(self, rva):
        start = self.offset(rva)
        return self.data[start:self.data.index(b'\0', start)].decode('ascii')

    def directory(self, index):
        rva = struct.unpack_from('<I', self.data, self.directories + 8 * index)[0]
        return self.offset(rva) if rva else None

    def imports(self):
        """The DLLs it imports, delay-loaded ones included."""
        names = []
        for index, size, field in ((1, 20, 3), (13, 32, 1)):
            cursor = self.directory(index)
            while cursor is not None:
                entry = struct.unpack_from(f'<{size // 4}I', self.data, cursor)
                if not any(entry):
                    break
                names.append(self.string(entry[field]))
                cursor += size
        return names

    def exports(self):
        """The names it exports, each with whether it names code (a function) or data."""
        cursor = self.directory(0)
        if cursor is None:
            return []
        functions, names, ordinals = struct.unpack_from('<III', self.data, cursor + 28)
        count = struct.unpack_from('<I', self.data, cursor + 24)[0]
        functions, names, ordinals = (self.offset(rva) for rva in (functions, names, ordinals))
        exported = []
        for index in range(count):
            name = self.string(struct.unpack_from('<I', self.data, names + 4 * index)[0])
            ordinal = struct.unpack_from('<H', self.data, ordinals + 2 * index)[0]
            rva = struct.unpack_from('<I', self.data, functions + 4 * ordinal)[0]
            exported.append((name, self.code(rva)))
        return exported


def imports(data):
    """The PE machine and the DLLs a Windows executable imports, delay-loaded ones included."""
    executable = Executable(data)
    return executable.machine, executable.imports()


def merge(wheels):
    """One tree from the wheels' payloads, keyed by its path. A path two wheels carry differently
    (their Python modules, a library both bundle) is left out, so the closure cannot take one."""
    tree, differing = {}, set()
    for package, data in wheels.items():
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            for member in archive.namelist():
                prefix = f'{package}-{VERSION}.data/purelib/{package}/'
                if not member.startswith(prefix) or member.endswith('/'):
                    continue
                relative = member[len(prefix):]
                content = archive.read(member)
                if tree.get(relative, content) != content:
                    differing.add(relative)
                tree[relative] = content
    for relative in differing:
        del tree[relative]
    return tree


def closure(tree):
    """The staged binaries, keyed by their path in the tree, and the Windows DLLs they import."""
    libraries = {Path(path).name.lower(): path for path in tree
                 if path.startswith('bin/') and path.endswith('.dll')}
    queue = [f'lib/gstreamer-1.0/gst{name}.dll' for name in sorted(PLUGINS)] + [SCANNER]
    staged, system = set(), set()
    while queue:
        path = queue.pop()
        if path in staged:
            continue
        if path not in tree:
            raise ValueError(f'GStreamer {VERSION} has no {path}')
        machine, names = imports(tree[path])
        if machine != AMD64:
            raise ValueError(f'{path} is not an x86_64 binary')
        staged.add(path)
        for name in names:
            if name.lower() in libraries:
                queue.append(libraries[name.lower()])
            else:
                system.add(name.lower())
    return staged, system


def review(staged, system):
    """Refuse a closure that is not the reviewed one, or that carries GPL code or an encoder."""
    libraries = {Path(path).name for path in staged if path.startswith('bin/')}
    expected = {Path(path).name: path for path in staged}
    for name, (licence, _) in LIBRARIES.items():
        if REFUSED_LICENCE.search(licence):
            raise ValueError(f'{name} is under the {licence}')
    for name, licence in PLUGINS.items():
        if REFUSED_LICENCE.search(licence):
            raise ValueError(f'the {name} plugin is under the {licence}')
    encoders = sorted(name for name in expected if ENCODERS.search(Path(name).stem))
    if encoders:
        raise ValueError(f'an encoder library in the closure: {encoders}')
    if {name.lower() for name in libraries} != {name.lower() for name in LIBRARIES}:
        unreviewed = sorted(n for n in libraries if n.lower() not in {k.lower() for k in LIBRARIES})
        gone = sorted(n for n in LIBRARIES if n.lower() not in {k.lower() for k in libraries})
        raise ValueError(f'the closure is not the reviewed one: unreviewed {unreviewed}, gone {gone}')
    # A Windows DLL the closure imports must be one every Windows 10 and later has.
    shipped = re.compile(r'^(api-ms-win-|ext-ms-)')
    foreign = sorted(name for name in system if not shipped.match(name) and name not in WINDOWS)
    if foreign:
        raise ValueError(f'the closure imports DLLs neither it nor Windows carries: {foreign}')


# The system DLLs the closure may import: each is part of every Windows 10 and later.
WINDOWS = {
    'advapi32.dll', 'avrt.dll', 'bcrypt.dll', 'crypt32.dll', 'd2d1.dll', 'd3d11.dll',
    'd3dcompiler_47.dll', 'dnsapi.dll', 'dwmapi.dll', 'dxgi.dll', 'gdi32.dll', 'iphlpapi.dll',
    'kernel32.dll', 'mf.dll', 'mfplat.dll', 'mfreadwrite.dll', 'mmdevapi.dll', 'ncrypt.dll',
    'ntdll.dll', 'ole32.dll', 'oleaut32.dll', 'secur32.dll', 'shell32.dll', 'shlwapi.dll',
    'user32.dll', 'userenv.dll', 'winmm.dll', 'ws2_32.dll', 'bcryptprimitives.dll', 'runtimeobject.dll',
}


def notices():
    names = {notice for _, files in LIBRARIES.values() for notice in files}
    names |= {notice for files in LINKED.values() for notice in files}
    sources = {entry['File']: entry['SHA256'] for entry in
               json.loads((NOTICES / 'sources.json').read_text())}
    texts = {}
    for name in sorted(names):
        data = (NOTICES / name).read_bytes()
        if name not in sources or digest(data) != sources[name]:
            raise ValueError(f'the notice {name} is not the one sources.json pins')
        texts[f'licenses/{name}'] = data
    return texts


def sdk(files):
    """The import libraries and pkg-config files a cross-build links the staged DLLs through."""
    out = {}
    with tempfile.TemporaryDirectory() as scratch:
        for name, (dll, version) in PKGCONFIG.items():
            exports = Executable(files[f'bin/{dll}']).exports()
            definition = Path(scratch, f'{name}.def')
            definition.write_text(f'LIBRARY {dll}\nEXPORTS\n' + ''.join(
                f'    {symbol}{"" if code else " DATA"}\n' for symbol, code in exports))
            library = Path(scratch, f'{name}.lib')
            subprocess.run(['llvm-dlltool', '-m', 'i386:x86-64', '-D', dll, '-d', str(definition),
                            '-l', str(library)], check=True)
            out[f'lib/{name}.lib'] = library.read_bytes()
            out[f'lib/pkgconfig/{name}.pc'] = (
                'prefix=${pcfiledir}/../..\nlibdir=${prefix}/lib\n\n'
                f'Name: {name}\nDescription: {dll} of GStreamer {VERSION}\'s MSVC build\n'
                f'Version: {version}\nLibs: -L${{libdir}} -l{name}\nCflags:\n').encode()
    return out


def pack(path, files):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w') as archive:
        for name in sorted(files):
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(files[name]), 0o644
            archive.addfile(info, io.BytesIO(files[name]))
    subprocess.run(['zstd', '-T4', '-19', '--quiet', '-f', '-o', str(path)],
                   input=buffer.getvalue(), check=True)


def fetch():
    wheels = {}
    for package, (url, sha256) in WHEELS.items():
        with urllib.request.urlopen(url, timeout=300) as response:
            data = response.read()
        if digest(data) != sha256:
            raise ValueError(f'{url} is not the pinned wheel')
        wheels[package] = data
    return wheels


def stage(out, wheels):
    tree = merge(wheels)
    staged, system = closure(tree)
    review(staged, system)
    files = {path: tree[path] for path in staged}
    files.update(notices())
    out.mkdir(parents=True, exist_ok=True)
    name = 'gstreamer-runtime-x86_64'
    pack(out / f'{name}.tar.zst', files)
    pack(out / 'gstreamer-sdk-x86_64.tar.zst', sdk(files))
    by_name = {key.lower(): value for key, value in LIBRARIES.items()}
    manifest = {
        'version': VERSION,
        'wheels': {package: sha256 for package, (_, sha256) in WHEELS.items()},
        'plugins': PLUGINS,
        'files': {path: {'sha256': digest(files[path]), **(
            {'licence': by_name[Path(path).name.lower()][0],
             'notices': by_name[Path(path).name.lower()][1]} if path.startswith('bin/') else {})}
            for path in sorted(files)},
        'linked': LINKED,
        'system_imports': sorted(system),
    }
    (out / f'{name}.json').write_text(json.dumps(manifest, indent=2) + '\n')
    for path in (out / f'{name}.tar.zst', out / f'{name}.json', out / 'gstreamer-sdk-x86_64.tar.zst'):
        Path(f'{path}.sha256').write_text(f'{digest(path.read_bytes())}  {path.name}\n')
    return len(staged)


def main(argv):
    if len(argv) != 2 or argv[0] != 'stage':
        print(__doc__, file=sys.stderr)
        return 2
    try:
        count = stage(Path(argv[1]), fetch())
    except (ValueError, OSError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        print(f'gstreamer runtime: {error}', file=sys.stderr)
        return 1
    print(f'staged {count} binaries of GStreamer {VERSION} for x86_64 Windows')
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
