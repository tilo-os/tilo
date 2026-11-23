"""Reviewed media additions; no automatic plugin or package expansion."""
from pathlib import Path
import time

import collection
import collection_local_runtime as runtime

PLUGINS = {'coreelements', 'app', 'playback', 'typefindfunctions', 'videoconvertscale',
           'videofilter', 'deinterlace', 'audioconvert', 'audioresample', 'autodetect',
           'osxaudio', 'volume', 'audioparsers', 'videoparsersbad', 'isomp4', 'matroska',
           'subparse', 'pango', 'assrender', 'reqwest'}
LIBRARIES = {'libass.9.dylib', 'libunibreak.8.dylib', 'libharfbuzz.0.dylib',
             'libgraphite2.3.3.1.dylib', 'libfreetype.6.dylib', 'libpng16.16.dylib',
             'libfribidi.0.dylib', 'libcairo.2.dylib', 'libpixman-1.0.dylib',
             'libxcb-shm.0.0.0.dylib', 'libXdmcp.6.dylib', 'libXau.6.dylib',
             'libxcb.1.1.0.dylib', 'libxcb-render.0.0.0.dylib', 'libXrender.1.dylib',
             'libX11.6.dylib', 'libXext.6.dylib', 'libfontconfig.1.dylib',
             'libpango-1.0.0.dylib', 'libthai.0.dylib', 'libdatrie.1.dylib',
             'libpangocairo-1.0.0.dylib', 'libpangoft2-1.0.0.dylib',
             'libgstriff-1.0.0.dylib', 'libgstrtp-1.0.0.dylib',
             'libgstcodecparsers-1.0.0.dylib'}
PACKAGES = {'gstreamer', 'libass', 'libunibreak', 'harfbuzz', 'graphite2', 'freetype',
            'libpng', 'fribidi', 'cairo', 'pixman', 'libxcb', 'libxdmcp', 'libxau',
            'libxrender', 'libx11', 'libxext', 'fontconfig', 'pango', 'libthai', 'libdatrie'}
DATA = {'libthai': 'share/libthai/thbrk.tri', 'libunibreak': 'LICENCE'}
PLUGIN_ADDITIONS = {
    'video-editor': {'avi', 'x264', 'opus', 'ogg', 'wavparse', 'aiff', 'asf', 'flac',
                     'flv', 'mpegpsdemux', 'mpegtsdemux', 'id3demux', 'apetag'},
}
LIBRARY_ADDITIONS = {
    'video-editor': {'libgstmpegts-1.0.0.dylib', 'libFLAC.14.dylib', 'libogg.0.8.6.dylib',
                     'libgstsdp-1.0.0.dylib', 'libgstrtsp-1.0.0.dylib'},
}
PACKAGE_ADDITIONS = {'video-editor': {'flac', 'libogg'}}
# The tilolang runtime's closure (collection_local_runtime.py `tilolang`): a player, so decoders
# only, and none under the GPL. H.264, HEVC, AAC and MP3 decode through the system's VideoToolbox
# and AudioToolbox (applemedia's vtdec and atdec), AV1 through dav1d; every library and Homebrew
# package they and the runtime load is named. A closure with anything more or less is refused.
TILOLANG_PLUGINS = {f'libgst{name}.dylib' for name in PLUGINS} | {'libgstapplemedia.dylib',
                                                                'libgstdav1d.dylib'}
TILOLANG_LIBRARIES = {
    'libX11.6.dylib', 'libXau.6.dylib', 'libXdmcp.6.dylib', 'libXext.6.dylib',
    'libXrender.1.dylib', 'libass.9.dylib', 'libbrotlicommon.1.2.1.dylib',
    'libbrotlidec.1.2.1.dylib', 'libcairo.2.dylib', 'libdatrie.1.dylib', 'libdav1d.7.dylib',
    'libfontconfig.1.dylib', 'libfreetype.6.dylib', 'libfribidi.0.dylib', 'libgio-2.0.0.dylib',
    'libglib-2.0.0.dylib', 'libgmodule-2.0.0.dylib', 'libgobject-2.0.0.dylib',
    'libgraphite2.3.3.1.dylib', 'libgstallocators-1.0.0.dylib', 'libgstapp-1.0.0.dylib',
    'libgstaudio-1.0.0.dylib', 'libgstbase-1.0.0.dylib', 'libgstcodecparsers-1.0.0.dylib',
    'libgstpbutils-1.0.0.dylib', 'libgstreamer-1.0.0.dylib', 'libgstriff-1.0.0.dylib',
    'libgstrtp-1.0.0.dylib', 'libgsttag-1.0.0.dylib', 'libgstvideo-1.0.0.dylib',
    'libharfbuzz.0.dylib', 'libintl.8.dylib', 'liborc-0.4.0.dylib', 'libpango-1.0.0.dylib',
    'libpangocairo-1.0.0.dylib', 'libpangoft2-1.0.0.dylib', 'libpcre2-8.0.dylib',
    'libpixman-1.0.dylib', 'libpng16.16.dylib', 'libthai.0.dylib', 'libunibreak.8.dylib',
    'libxcb-render.0.0.0.dylib', 'libxcb-shm.0.0.0.dylib', 'libxcb.1.1.0.dylib',
}
TILOLANG_PACKAGES = set()


def expected_packages(name):
    if name not in PACKAGE_ADDITIONS:
        raise ValueError('unreviewed media app')
    return PACKAGES | PACKAGE_ADDITIONS[name]


def additional_assets(plan, profile):
    """Pin local data and notices, using system fonts and system TLS trust."""
    deadline = time.monotonic() + 30
    assets = plan['assets']
    packages = {Path(root).parent.name: record for root, record in profile['packages'].items()}
    expected = expected_packages(profile.get('app'))
    if len(packages) != len(profile['packages']) or set(packages) != expected:
        raise ValueError('media package evidence must match the app without duplicate versions')
    for name in sorted(expected - {'gstreamer'}):
        record = packages[name]
        root = Path(record['package_root'])
        notices = record['top_level_notice_candidates']
        if not notices and name != 'libunibreak':
            raise ValueError('media package requires recorded notices: ' + name)
        for item in notices:
            source = Path(item['path'])
            if collection.digest(source) != item['sha256']:
                raise ValueError('media notice changed')
            runtime.add_asset(assets, source, f'Contents/Resources/Notices/{name}/{source.name}',
                              (root,), deadline)
    if set(profile.get('data_assets', {})) != set(DATA):
        raise ValueError('media profile requires exactly its reviewed data assets')
    for name, relative in DATA.items():
        root = Path(packages[name]['package_root'])
        source = root / relative
        recorded = profile['data_assets'][name]
        if recorded != {'source': str(source), 'sha256': collection.digest(source)}:
            raise ValueError('media dictionary or notice changed')
        target = ('Contents/Resources/libthai/thbrk.tri' if name == 'libthai' else
                  'Contents/Resources/Notices/libunibreak/LICENCE')
        runtime.add_asset(assets, source, target, (root,), deadline)
    config = Path(__file__).with_name('local_media_fonts.conf')
    runtime.add_asset(assets, config, 'Contents/Resources/fontconfig/fonts.conf',
                      (config.parent,), deadline)
    plan['media_data'] = True
