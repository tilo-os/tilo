#!/usr/bin/env bash
# Produce the independently signed worker before compiling its host Imager.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
phase=all
expected_worker=6
build_packages=imager
# Explicit separate cohort; assemble-only must prove its actual binary matches.
if [ "${1:-}" = --applications-worker ]; then expected_worker=6; shift; fi
# Full shared-file qualification uses the ordinary Writer from the same graph.
if [ "${1:-}" = --with-writer ]; then build_packages="imager file-explorer word-processor browser"; shift; fi
case "${1:-}" in
    --build-only) phase=build; shift ;;
    --assemble-only) phase=assemble; shift ;;
esac
out=${1:?usage: package-capture-service.sh [--applications-worker] [--with-writer] [--build-only|--assemble-only] OUTPUT PINS_JSON}
pins=${2:?missing generated pins output}
service_version=${TILO_VERSION:-}
service_version=${service_version#imager-v}
service_version=${service_version#v}
[ "$(uname -s)" = Linux ] || { echo 'capture service is Linux-only' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
features=$(python3 - "$repo/dist/apps/app-collections.toml" <<'PY_FEATURES'
import sys, tomllib
value = tomllib.load(open(sys.argv[1], 'rb'))
print(','.join(package+'/'+feature for package, features in value['features'].items() for feature in features))
PY_FEATURES
)
if [ "$expected_worker" = 6 ]; then
    features="${features:+$features,}imager/capture-worker-applications"
fi
if [ "$phase" != assemble ]; then
BUILD_PHASE=build BUILD_PACKAGES="$build_packages" BUILD_ROLE=home-capture APP_FEATURES="$features" \
    "$repo/os/scripts/build-thin-apps.sh"
"${CARGO:-cargo}" build --locked --release -p launcher --bin tilo-launcher -p runtime-bundle --bin tilo-runtime-bundle \
    -p tilo-app-package-cli --bin tilo-app-package -p install --bin tilo-install
fi
if [ "$phase" = build ]; then
    printf 'capture artifacts compiled; run --assemble-only in their native builder\n'
    exit 0
fi
[ ! -e "$out" ] && [ ! -L "$out" ] && [ ! -e "$pins" ] && [ ! -L "$pins" ] \
    || { echo 'capture assembly outputs must be fresh' >&2; exit 1; }
[[ "$service_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]] \
    || { echo 'an explicit valid TILO_VERSION is required for capture assembly' >&2; exit 1; }
runtime_identity=${TILO_CAPTURE_RUNTIME_IDENTITY:-}
service_identity=${TILO_CAPTURE_SERVICE_IDENTITY:-}
# Signing authority is operator-provided. This producer never creates release
# identities or promotes a candidate to a qualified migration service.
[ -n "$runtime_identity" ] && [ -n "$service_identity" ] \
    || { echo 'explicit capture runtime and service signing identities are required' >&2; exit 1; }
for identity in "$runtime_identity" "$service_identity"; do
    [ -f "$identity" ] && [ ! -L "$identity" ] \
        || { echo 'capture signing identity is missing or redirected' >&2; exit 1; }
done
"$repo/apps/imager/packaging/capture-native-preflight.sh"
# Native-only protocol verification uses this exact full thin graph's SDK and Rust std.
# It invokes the headless worker and SDK protocol probe, never the installer.
BUILD_PHASE=verify BUILD_PACKAGES="$build_packages" BUILD_ROLE=home-capture APP_FEATURES="$features" \
    "$repo/os/scripts/build-thin-apps.sh"
producer="$repo/target/release/tilo-runtime-bundle"
packer="$repo/target/release/tilo-app-package"
worker="$repo/target/thin-app/tilo-home-export"
sdk="$repo/target/thin-app/libruntime_dylib.so"
probe="$repo/target/thin-app/tilo-runtime-account-capability"
rust_libdir=$(rustc --print target-libdir)
std=$(find "$rust_libdir" -maxdepth 1 -name 'libstd-*.so' -print -quit)
[ -n "$std" ] || { echo 'matching Rust standard library missing' >&2; exit 1; }
launcher="$repo/target/release/tilo-launcher"
bwrap=/usr/bin/bwrap
tar=/usr/bin/tar
# Probe the exact configured GPGME OpenPGP engine, never guess gpg/gpg2.
# This executable is trusted native builder tooling, not a worker-selected command.
openpgp_probe="$work/tilo-capture-openpgp-probe"
"${CC:-cc}" -Wall -Wextra -Werror \
    $(pkg-config --cflags gpgme) "$repo/apps/imager/packaging/capture-openpgp-engine.c" \
    $(pkg-config --libs gpgme) -o "$openpgp_probe"
python3 "$repo/apps/imager/packaging/capture_openpgp.py" "$openpgp_probe" \
    "$work/tilo-capture-openpgp.json" "$work/openpgp-sources"
openpgp_helpers=()
while IFS= read -r helper; do openpgp_helpers+=("$helper"); done < "$work/openpgp-sources"
for executable in "$worker" "$launcher" "$bwrap" "$tar"; do
    [ -f "$executable" ] && [ ! -L "$executable" ] && [ -x "$executable" ] \
        || { echo 'capture runtime executable is missing or redirected' >&2; exit 1; }
done
runtime_publisher=$("$producer" key pubkey --identity "$runtime_identity")
service_publisher=$("$producer" key pubkey --identity "$service_identity")
mkdir -p "$out/runtime" "$out/service/metadata" "$work/payload/bin"
install -pm0755 "$worker" "$work/payload/bin/tilo-home-export"
# The canonical runtime collector observes both transitive DT_NEEDED and every
# .tilo.lazy_needed library the SDK opens on first use, including their closure.
# Capture its status directly so a failed discovery can never be hidden by a
# process substitution and signed as an incomplete runtime.
dependencies=$(python3 "$repo/os/scripts/collect-runtime-dependencies.py" \
    --library-dir "$(dirname "$sdk")" --library-dir "$rust_libdir" \
    "$worker" "$sdk" "$std" "$launcher" "$bwrap" "$probe" "$tar" "$openpgp_probe" "${openpgp_helpers[@]}") \
    || { echo 'capture runtime has an unresolved native or first-use dependency closure' >&2; exit 1; }
includes=(--include "$sdk" --include "$std" --include "$work/tilo-capture-openpgp.json")
helper_bins=(--include-bin "$openpgp_probe")
for helper in "${openpgp_helpers[@]}"; do helper_bins+=(--include-bin "$helper"); done
while IFS= read -r dependency; do includes+=(--include "$dependency"); done <<< "$dependencies"
"$producer" build --release 2026.07 --arch "$(uname -m)" --from / \
    --identity "$runtime_identity" --include-bin "$launcher" --include-bin "$bwrap" --include-bin "$probe" --include-bin "$tar" \
    "${helper_bins[@]}" "${includes[@]}" --out-root "$out/runtime" --version-out "$work/runtime-version"
version=$(cat "$work/runtime-version")
"$producer" verify "$out/runtime/$version" --require-publisher "$runtime_publisher" \
    --require-signature --require-arch "$(uname -m)"
bash "$repo/os/scripts/verify-runtime-bundle-closure.sh" "$out/runtime/$version" "$worker"
bash "$repo/os/scripts/verify-runtime-bundle-closure.sh" "$out/runtime/$version" "$out/runtime/$version/bin/tar"
# Every subprocess engine is independently signed and closure checked.
while IFS= read -r helper; do
    bash "$repo/os/scripts/verify-runtime-bundle-closure.sh" "$out/runtime/$version" \
        "$out/runtime/$version/bin/$(basename "$helper")"
done < "$work/openpgp-sources"
# Resolve the actual SDK probe from the verified runtime, using its own signed
# interpreter and library closure rather than the builder's installed libraries.
python3 - "$repo" "$out/runtime/$version" "$work/payload/bin/tilo-home-export" "$work/worker-protocol" "$expected_worker" <<'PY_PROBE'
import pathlib, sys, tomllib
repo, bundle, worker, protocol_output = map(pathlib.Path, sys.argv[1:5])
expected_worker = int(sys.argv[5])
sys.path.insert(0, str(repo/'apps/imager/packaging'))
import runtime_probe as verifier
probe = bundle/'bin/tilo-runtime-account-capability'
loader_name = verifier.interpreter(probe)
manifest = tomllib.loads((bundle/'bundle.toml').read_text())
verifier.require(any(entry['path'] == loader_name for entry in manifest['files']),
                 'capture probe interpreter is absent from signed runtime')
verifier.protocol(verifier.invoke([str(bundle/loader_name), '--inhibit-cache',
                                  '--library-path', str(bundle/'lib'), str(probe)],
                                 {'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'}))
tar = bundle/'bin/tar'
tar_loader = verifier.interpreter(tar)
verifier.require(tar_loader == loader_name, 'capture archive helper uses another interpreter')
result = verifier.invoke([str(bundle/tar_loader), '--inhibit-cache', '--library-path',
                          str(bundle/'lib'), str(tar), '--version'],
                         {'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'})
verifier.require(b'GNU tar' in result, 'capture archive helper is not GNU tar')
import capture_protocol
worker_loader = verifier.interpreter(worker)
verifier.require(worker_loader == loader_name, 'capture worker uses another interpreter')
worker_protocol = capture_protocol.for_cohort(verifier.invoke(
    [str(bundle/worker_loader), '--inhibit-cache', '--library-path', str(bundle/'lib'), str(worker), '--protocol'],
    {'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'}), expected_worker)
protocol_output.write_text(str(worker_protocol))
import json
helper_table = json.loads((bundle/'lib/tilo-capture-openpgp.json').read_bytes())
for helper in helper_table['helpers']:
    path = bundle/helper['bundle']
    verifier.require(verifier.interpreter(path) == loader_name,
                     'capture OpenPGP helper uses another interpreter')
    result = verifier.invoke([str(bundle/loader_name), '--inhibit-cache', '--library-path',
                              str(bundle/'lib'), str(path), '--version'],
                             {'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'})
    if helper['role'] == 'openpgp':
        verifier.require(helper_table['openpgp_version'].encode() in result,
                         'signed OpenPGP engine differs from canonical GPGME version')
print('capture signed SDK, GNU tar and configured OpenPGP ELF probes passed')
PY_PROBE
worker_protocol=$(cat "$work/worker-protocol")
cat > "$work/manifest.toml" <<EOF
[package]
id = "so.tilo.imager.home-export"
version = "$service_version"
kind = "service"
runtime = "tilo-runtime/$version"
[package.service-protocol]
name = "tilo.home-capture"
version = $worker_protocol
[package.isa]
x86_64 = "baseline"
aarch64 = "armv8-a"
[publisher]
identity = "ed25519:$service_publisher"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/tilo-home-export"
type = "cli"
EOF
python3 "$repo/tools/apps/app_package_isa.py" stage --arch "$(uname -m)" \
    --manifest "$work/manifest.toml" --binary "$worker" --output "$work/manifest.toml"
"$packer" pack --manifest "$work/manifest.toml" --payload "$work/payload" \
    --identity "$service_identity" --output "$out/service/home-export.tapp"
"$packer" verify --trusted-key "$service_publisher" "$out/service/home-export.tapp"
"$packer" unpack --dest "$out/service/payload" "$out/service/home-export.tapp"
# CLI unpack also writes a convenience manifest, which is not payload data.
rm "$out/service/payload/manifest.toml"
python3 - "$out" "$pins" "$version" "$runtime_publisher" "$service_publisher" "$worker_protocol" <<'PY'
import hashlib, json, pathlib, platform, tarfile, tomllib, sys
out, pins, version, runtime_publisher, service_publisher, observed_protocol = sys.argv[1:]
observed_protocol = int(observed_protocol)
root = pathlib.Path(out)
with tarfile.open(root/'service/home-export.tapp') as archive:
    def read(name, limit):
        entries = [entry for entry in archive.getmembers() if entry.name == name]
        if len(entries) != 1 or not entries[0].isfile() or entries[0].size > limit:
            raise SystemExit('invalid signed service metadata')
        return archive.extractfile(entries[0]).read()
    header = tomllib.loads(read('tapp.toml', 4096).decode())
    signed = read('signed-root', 173)
    manifest = read('manifest.toml', 1024*1024)
    protocol = tomllib.loads(manifest.decode())['package']['service-protocol']
    if protocol != {'name':'tilo.home-capture', 'version':observed_protocol}:
        raise SystemExit('signed capture worker protocol differs from observed executable')
    collection = read('collection.qcl', 16*1024*1024)
    if len(signed) != 173 or header['format'] != 1 or not header['package_id'].startswith('tapp:'):
        raise SystemExit('invalid signed service identity')
    for name, value in [('signed-root',signed),('manifest.toml',manifest),('collection.qcl',collection)]:
        (root/'service/metadata'/name).write_bytes(value)
runtime = tomllib.loads((root/'runtime'/version/'bundle.toml').read_text())
value = dict(schema=1, worker_protocol=protocol['version'], arch=platform.machine(), package_id='so.tilo.imager.home-export',
             service_publisher=service_publisher, package_hash=header['package_id'][5:],
             manifest_sha256=hashlib.sha256(manifest).hexdigest(), payload_root_hash=signed[44:76].hex(),
             runtime_version=version, runtime_publisher=runtime_publisher, runtime_root_hash=runtime['root_hash'])
pathlib.Path(pins).write_text(json.dumps(value, sort_keys=True, separators=(',',':'))+'\n')
PY
