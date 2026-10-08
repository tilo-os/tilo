"""Explicit per-app additions to the private local runtime profile."""
from __future__ import annotations

import copy
from pathlib import Path
import time

import collection
import collection_local_runtime as runtime
import collection_media_runtime as media
from collection_native import system_library
from collection_runtime import formula_evidence

PLUGINS = {
    **{name: media.PLUGINS | extra for name, extra in media.PLUGIN_ADDITIONS.items()},
    'remote': {'coreelements', 'app', 'videoconvertscale', 'videoparsersbad'},
    'music': {'coreelements', 'app', 'playback', 'typefindfunctions', 'audioconvert',
              'audioresample', 'autodetect', 'osxaudio', 'volume', 'audiofx', 'audioparsers',
              'ogg', 'isomp4', 'matroska', 'wavparse', 'aiff', 'asf', 'id3demux', 'apetag'},
}
LIBRARIES = {
    **{name: media.LIBRARIES | extra for name, extra in media.LIBRARY_ADDITIONS.items()},
    'remote': {'libgstcodecparsers-1.0.0.dylib'},
    'music': {'libgstsdp-1.0.0.dylib', 'libgstrtp-1.0.0.dylib', 'libgstrtsp-1.0.0.dylib',
              'libgstriff-1.0.0.dylib', 'libogg.0.8.6.dylib', 'libgstfft-1.0.0.dylib'},
}
DOMAINS = {'gst-plugins-good-1.0', 'gst-plugins-bad-1.0', 'gst-plugins-ugly-1.0'}


def additional_assets(plan, profile, name):
    deadline = time.monotonic() + 30
    assets = plan['assets']
    if name in media.PLUGIN_ADDITIONS:
        media.additional_assets(plan, profile)
    if name != 'music' and name not in media.PLUGIN_ADDITIONS:
        return
    if name == 'music':
        music_notices(plan, profile, deadline)
    locale = Path(plan['locale_prefixes']['gstreamer'])
    found = set()
    domains = DOMAINS if name in ('music', 'video-editor') else {'gst-plugins-good-1.0'}
    for language in runtime.directory_entries(locale, 512, deadline):
        if language.is_symlink():
            raise ValueError('symlink locale directory')
        if not language.is_dir():
            continue
        messages = language / 'LC_MESSAGES'
        if messages.is_symlink():
            raise ValueError('symlink message directory')
        if not messages.is_dir():
            continue
        for domain in sorted(domains):
            source = messages / (domain + '.mo')
            if source.exists():
                target = 'Contents/Resources/locale/' + str(source.relative_to(locale))
                runtime.add_asset(assets, source, target, (locale,), deadline)
                found.add(domain)
    if found != domains:
        raise ValueError('missing additional GStreamer translation catalogs')
    plan['extended_locale_catalogs'] = True


def music_notices(plan, profile, deadline):
    package = next(record for root, record in profile['packages'].items()
                   if Path(root).parent.name == 'libogg')
    notices = package['top_level_notice_candidates']
    if not notices:
        raise ValueError('libogg requires its recorded local notices')
    for item in notices:
        source = Path(item['path'])
        if collection.digest(source) != item['sha256']:
            raise ValueError('libogg notice changed')
        runtime.add_asset(plan['assets'], source, 'Contents/Resources/Notices/libogg/' + source.name,
                          (Path(package['package_root']),), deadline)


def prepare(path, expected, entry, binary, base):
    profile = runtime.checked_json(path, expected)
    name = entry['name']
    if (name not in PLUGINS or profile.get('app') != name or profile.get('format') != 1 or
            profile.get('source') != base['source'] or
            profile.get('base_profile_sha256') != base['profile_sha256']):
        raise ValueError('app profile must match the exact source, app and base profile')
    plugin_names = {'libgst' + plugin + '.dylib' for plugin in PLUGINS[name]}
    gst_root = Path(base['locale_prefixes']['gstreamer']).parent.parent
    plugin_paths = {str(gst_root / 'lib/gstreamer-1.0' / plugin) for plugin in plugin_names}
    if set(profile['roots']) != plugin_paths or set(profile['plugins']) != plugin_names:
        raise ValueError('app plugin set differs from the reviewed delta')
    images = profile['images']
    if (len(images) != len(plugin_names) + len(LIBRARIES[name]) or
            {Path(source).name for source in images} != plugin_names | LIBRARIES[name]):
        raise ValueError('app library set differs from the reviewed delta')
    plan = copy.deepcopy(base)
    destinations = {item['destination'].casefold() for item in plan['images'].values()}
    packages = {}
    deadline = time.monotonic() + 120
    for source, recorded in images.items():
        original = Path(source)
        file = runtime.regular_file(original, runtime.LIMIT, (Path('/opt/homebrew/Cellar'),))
        if str(file) != source:
            raise ValueError('app profile requires canonical library paths')
        facts = runtime.image_info(file, 'arm64', min(15, runtime.remaining(deadline)))
        if facts != recorded or runtime.version(facts['slices']['arm64']['minimum_macos']) > (27, 0, 0):
            raise ValueError('app profile input changed or exceeds macOS 27')
        if facts['slices']['arm64']['filetype'] not in ('DYLIB', 'BUNDLE'):
            raise ValueError('app profile contains a non-library image')
        package = formula_evidence(file, (Path('/opt/homebrew'),), deadline)
        if not package or package != profile['packages'].get(package['package_root']):
            raise ValueError('app library package evidence changed')
        packages[package['package_root']] = package
        folder = 'PlugIns' if source in plugin_paths else 'Frameworks'
        target = f'Contents/{folder}/{file.name}'
        if source in plan['images'] or target.casefold() in destinations:
            raise ValueError('app runtime filename collision')
        destinations.add(target.casefold())
        plan['images'][source] = {'destination': target, **facts}
    expected_packages = {'gstreamer', 'libogg'} if name == 'music' else {'gstreamer'}
    if set(profile['packages']) != set(packages):
        raise ValueError('app profile includes unused package evidence')
    if name in media.PLUGIN_ADDITIONS:
        expected_packages = media.expected_packages(name)
    if {Path(root).parent.name for root in packages} != expected_packages:
        raise ValueError('app profile added an unreviewed package')
    limit = 24 if name in media.PLUGIN_ADDITIONS else 8
    if sum(item['bytes'] for item in images.values()) > limit * 1024**2:
        raise ValueError('app runtime delta exceeds its byte bound')
    main_name = entry['binary']
    if main_name != name:
        raise ValueError('reviewed app entrypoint changed')
    main_target = 'Contents/MacOS/' + main_name + '.real'
    if main_target.casefold() in destinations:
        raise ValueError('real app executable collision')
    plan['images'][binary['path']] = {'destination': main_target, **binary['facts']}
    runtime.relocation_map(plan['images'])
    plan.update(scope='private-local-macos27-app', profile_sha256=expected,
                app_launcher='Contents/MacOS/' + main_name)
    additional_assets(plan, profile, name)
    return plan, {'profile_sha256': expected, 'additional_sources': sorted(images),
                  'main_source': binary['path'], 'main_real': main_target,
                  'adapter_sha256': collection.digest(Path(__file__)),
                  'media_adapter_sha256': collection.digest(Path(media.__file__)),
                  'runtime_qualified': False}


def augment(app, plan, guard):
    details = plan['app_runtime']
    if details['adapter_sha256'] != collection.digest(Path(__file__)):
        raise ValueError('app runtime adapter changed')
    if (plan['runtime_plan'].get('media_data') and
            details.get('media_adapter_sha256') != collection.digest(Path(media.__file__))):
        raise ValueError('media runtime adapter changed')
    main = app / plan['runtime_plan']['app_launcher']
    real = app / details['main_real']
    if real.exists() or collection.digest(main) != plan['binary_sha256']:
        raise ValueError('SDK app changed or real-executable destination exists')
    main.rename(real)
    runtime_plan = plan['runtime_plan']
    for source in details['additional_sources']:
        item = runtime_plan['images'][source]
        target = app / item['destination']
        runtime.copy_checked(Path(source), target, item['sha256'])
        target.chmod(0o755)
    for source in [*details['additional_sources'], details['main_source']]:
        item = runtime_plan['images'][source]
        command = ['/usr/bin/install_name_tool']
        for old, new in item['changes']:
            command += ['-change', old, new]
        arch = item['slices']['arm64']
        for rpath in arch['rpaths']:
            command += ['-delete_rpath', rpath]
        if arch['install_id']:
            command += ['-id', '@loader_path/' + Path(item['destination']).name]
        if len(command) > 1:
            runtime.run(command + [app / item['destination']])
    for relative, item in runtime_plan['assets'].items():
        if relative not in plan['base_runtime_plan']['assets']:
            runtime.copy_checked(Path(item['source']), app / relative, item['sha256'])
            (app / relative).chmod(0o644)
    runtime.compile_adapters(app, runtime_plan, guard)


def inspect_launcher(main):
    facts = runtime.image_info(main, 'arm64', 15)
    arch = facts['slices']['arm64']
    if (arch['filetype'] != 'EXECUTE' or arch['rpaths'] or
            runtime.version(arch['minimum_macos']) != runtime.version('27.0') or
            any(not system_library(edge['load']) for edge in arch['dependencies'])):
        raise ValueError('unexpected main launcher metadata or dependencies')
    return facts
