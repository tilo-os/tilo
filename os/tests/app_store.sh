#!/usr/bin/env bash
# Assert that the preinstalled application store can actually be built and
# will actually work.
#
#   ./os/tests/app_store.sh
#
# Three things here have no knowledge of each other and every one of them
# fails silently — as an empty launcher, or as an app that installs fine and
# then refuses to start:
#
#   * Every apps/<name>/tilo-app.toml declares `runtime = "tilo-runtime/<v>"`.
#     The launcher resolves that literal string to /usr/lib/tilo-runtime/<v>/
#     and errors with RuntimeBundleMissing if it is not there. The image bakes
#     exactly one version, from RUNTIME_VERSION in os/Containerfile. These
#     were "2026.07" and "1" respectively, so every packaged app would have
#     failed at launch.
#   * os/Containerfile derives the app package list from the `apps/…` entries
#     in the workspace members, and cargo takes *package* names there. A
#     directory whose package name differs would be silently absent from the
#     build, and its app silently absent from the image.
#   * os/scripts/pack-apps.sh finds each binary by the basename of the
#     manifest's `[entrypoint].bin`. A manifest naming a binary no crate
#     produces packs nothing.
#
# Exit codes: 0 all assertions hold; 1 at least one failed.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
CONTAINERFILE="$REPO/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$REPO/os/tests/lib/containerfile.sh"
PACK="$REPO/os/scripts/pack-apps.sh"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for f in "$CONTAINERFILE" "$PACK" "$REPO/Cargo.toml"; do
  [ -r "$f" ] || { bad "missing $f"; exit 1; }
done

manifests=("$REPO"/apps/*/tilo-app.toml)
[ -e "${manifests[0]}" ] || { bad "no apps/*/tilo-app.toml found"; exit 1; }

# --- 1. the baked runtime version is the one the apps ask for ----------------
baked="$(sed -n 's/^ARG RUNTIME_VERSION=\(.*\)$/\1/p' "$CONTAINERFILE" | head -1)"
if [ -z "$baked" ]; then
  bad "could not read ARG RUNTIME_VERSION — the declaration it is parsed out of moved"
else
  declared="$(sed -n 's/^runtime  *= *"tilo-runtime\/\([^"]*\)".*/\1/p' "${manifests[@]}" \
                | sort -u)"
  count="$(printf '%s\n' "$declared" | grep -c . || true)"
  if [ "$count" -ne 1 ]; then
    bad "apps declare $count different runtime versions, so no single bake can serve them:
      $(printf '%s' "$declared" | tr '\n' ' ')"
  elif [ "$declared" = "$baked" ]; then
    ok "runtime bundle bakes $baked, which is what every app manifest declares"
  else
    bad "os/Containerfile bakes tilo-runtime/$baked but the apps declare tilo-runtime/$declared
      The launcher looks for /usr/lib/tilo-runtime/$declared/ and will refuse every
      preinstalled app with RuntimeBundleMissing. Change ARG RUNTIME_VERSION in
      os/Containerfile, or the manifests, so the two agree."
  fi
fi

# --- 2. every app is a workspace member with a unique Cargo package name -----
missing_member=0
bad_name=0
declare -A app_package_names=()
for m in "${manifests[@]}"; do
  dir="$(dirname "$m")"
  rel="apps/$(basename "$dir")"
  grep -qF "\"$rel\"" "$REPO/Cargo.toml" || { bad "$rel is not a workspace member"; missing_member=1; }
  # build-thin-apps.sh reads this package name from Cargo.toml. It deliberately
  # need not equal the directory (apps/windows uses windows-app to avoid the
  # registry's `windows` package making `cargo -p windows` ambiguous).
  pkg="$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)"
  if [ -z "$pkg" ] || [ -n "${app_package_names[$pkg]:-}" ]; then
    bad "$rel has an empty or duplicate app package name '$pkg'"
    bad_name=1
  else
    app_package_names[$pkg]="$rel"
  fi
done
[ "$missing_member" = 1 ] || ok "all ${#manifests[@]} app manifests sit in workspace members"
[ "$bad_name" = 1 ] || ok "every app package name is present and unique, so -p resolves"

# --- 3. every manifest entrypoint names a binary some crate produces ---------
no_bin=0
for m in "${manifests[@]}"; do
  dir="$(dirname "$m")"
  rel="apps/$(basename "$dir")"
  entry="$(sed -n 's/^bin  *= *"\([^"]*\)".*/\1/p' "$m" | head -1)"
  if [ -z "$entry" ]; then
    bad "$rel: no [entrypoint].bin — pack-apps.sh cannot place a payload binary"
    no_bin=1
    continue
  fi
  want="$(basename "$entry")"
  # Either an explicit [[bin]] name, or the package name as the default bin.
  have="$(sed -n '/^\[\[bin\]\]/,/^$/s/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml")"
  [ -n "$have" ] || have="$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)"
  grep -qx "$want" <<<"$have" || {
    bad "$rel: manifest entrypoint is '$entry' but the crate builds [$(printf '%s' "$have" | tr '\n' ' ')]"
    no_bin=1
  }
done
[ "$no_bin" = 1 ] || ok "every manifest entrypoint names a binary its crate builds"

# Thin tapp binaries must all opt into the same shared-runtime edge. Merely
# building runtime-dylib is not enough: one forgotten app silently returns to a
# 20–30 MB static archive, and building apps separately creates incompatible
# Rust crate hashes even when every individual link succeeds.
thin_bad=0
for m in "${manifests[@]}"; do
  dir="$(dirname "$m")"
  grep -q '^thin = \["dep:runtime-dylib"\]$' "$dir/Cargo.toml" \
    || { bad "$(basename "$dir") does not define the shared-runtime thin feature"; thin_bad=1; }
  grep -q '^runtime-dylib = .*optional = true' "$dir/Cargo.toml" \
    || { bad "$(basename "$dir") does not make runtime-dylib an optional dependency"; thin_bad=1; }
done
[ "$thin_bad" = 1 ] || ok "all ${#manifests[@]} apps opt into one ABI-matched shared runtime"

# Cargo may compile two instances of the same dependency when their feature
# sets differ across a Rust dylib boundary. That links, but the mangled crate
# hashes differ and the loader later reports undefined symbols. Keep the
# runtime UI feature set at the collection-wide superset and require a loader
# smoke test before a bundle can be signed.
runtime_manifest="$REPO/crates/runtime/runtime-dylib/Cargo.toml"
thin_builder="$REPO/os/scripts/build-thin-apps.sh"
if grep -Fq 'features = ["portal", "webview-adblock", "dmabuf-import", "gpu-video", "mpris"]' \
      "$runtime_manifest" \
   && grep -Fq 'thin app/runtime ABI mismatch' "$thin_builder" \
   && grep -Fq 'symbol lookup error|undefined symbol|error while loading shared libraries' "$thin_builder"; then
  ok "thin apps and the shared runtime enforce one loadable UI ABI"
else
  bad "thin app packaging can still sign a runtime/app pair with incompatible Rust crate hashes"
fi

SIZE_POLICY="$REPO/crates/packaging/tilo-app-package/max-app-bytes"
policy_bytes=$(cat "$SIZE_POLICY")
if grep -q 'crates/packaging/tilo-app-package/max-app-bytes' "$PACK" \
   && ! grep -Eq 'POLICY_MAX_TAPP_BYTES=[0-9]' "$PACK" \
   && ! grep -q 'RECOVERY_MAX_TAPP_BYTES' "$PACK" \
   && grep -q 'include_str!("../max-app-bytes")' \
        "$REPO/crates/packaging/tilo-app-package/src/lib.rs"; then
  ok "pack-apps enforces the canonical tapp ceiling shared with the Rust checks, without app exceptions"
else
  bad "pack-apps does not enforce the canonical tapp ceiling from the shared policy file"
fi

weakening_output=$(MAX_TAPP_BYTES=$((policy_bytes + 1)) "$PACK" 2>&1)
weakening_status=$?
if [ "$weakening_status" -ne 0 ] \
   && grep -q "cannot exceed the $policy_bytes-byte application policy" <<<"$weakening_output"; then
  ok "pack-apps refuses an environment override that weakens the size policy"
else
  bad "MAX_TAPP_BYTES can weaken the canonical application size policy"
fi

if grep -q -- '--include /out/libruntime_dylib.so' "$CONTAINERFILE" \
   && grep -q 'ldd /out/libruntime_dylib.so' "$CONTAINERFILE"; then
  ok "the signed runtime bundle carries the shared runtime and its ELF dependencies"
else
  bad "the image builds thin apps but does not bundle their shared runtime dependencies"
fi

# --- 4. the store the builder writes is the store the image ships ------------
# pack-apps.sh writes registry.toml with absolute payload paths, so the path it
# installs to has to be the path the rootfs stage copies to, byte for byte.
store="$(sed -n 's/^STORE="\${STORE:-\(.*\)}"$/\1/p' "$PACK" | head -1)"
if [ -z "$store" ]; then
  bad "could not read the default STORE out of os/scripts/pack-apps.sh"
elif containerfile_has_copy "$CONTAINERFILE" "$store/" "$store/" builder; then
  ok "the image copies the app store from and to $store, so registry paths resolve"
else
  bad "os/Containerfile does not copy $store/ to $store/
      pack-apps.sh records absolute payload paths; copying the store anywhere
      else leaves every registry row pointing at a directory that is not there."
fi

# --- 5. the launcher actually reads that store -------------------------------
reg="$REPO/crates/packaging/package-store/src/layout.rs"
if [ ! -r "$reg" ]; then
  bad "missing $reg"
elif grep -q "SYSTEM_DATA_DIR: &str = \"$store\"" "$reg"; then
  ok "the package store's system store constant is $store"
else
  bad "$reg does not point SYSTEM_DATA_DIR at $store — the image would ship a
      store nothing reads."
fi

# --- 6. the runtime bundle is baked where tilo-install looks for it ------------
# pack-apps.sh installs each package with `tilo-install install`, and that refuses
# any package whose declared runtime is absent (satisfy_runtime ->
# RuntimeDependencyMissing). It only ever consults two directories: the per-user
# one, which does not exist in a container build, and the system one from
# runtime_bundle::system_bundle_path. So the bake has to land on the system path
# during the *builder* stage — not merely be copied there afterwards, which is a
# stage too late and left all 46 packages refused at install time.
#
# Checked against the constant rather than against a repeated literal, so moving
# system_bundle_path fails here instead of at the next image build.
BUNDLE_LIB="$REPO/crates/packaging/runtime-bundle/src/lib.rs"
INSTALL_MAIN="$REPO/crates/packaging/install/src/main.rs"
sysroot="$(sed -n 's|^ *PathBuf::from("\(/usr/lib/tilo-runtime\)").join(version)$|\1|p' \
             "$BUNDLE_LIB" | head -1)"
if [ ! -r "$BUNDLE_LIB" ] || [ ! -r "$INSTALL_MAIN" ]; then
  bad "missing $BUNDLE_LIB or $INSTALL_MAIN — cannot check where the bundle is looked up"
elif [ -z "$sysroot" ]; then
  bad "could not read the system bundle root out of runtime_bundle::system_bundle_path
      ($BUNDLE_LIB) — the declaration it is parsed out of moved, so this check
      cannot see its subject and must not pass."
else
  if grep -q 'system_bundle_path' "$INSTALL_MAIN"; then
    ok "tilo-install resolves a declared runtime against $sysroot"
  else
    bad "$INSTALL_MAIN no longer consults system_bundle_path, so baking into
        $sysroot would no longer satisfy the install-time runtime check."
  fi
  if grep -q -- "--out \"$sysroot/\${RUNTIME_VERSION}\"" "$CONTAINERFILE"; then
    ok "os/Containerfile bakes the runtime bundle into $sysroot/\${RUNTIME_VERSION}"
  else
    bad "os/Containerfile does not bake the runtime bundle into $sysroot/\${RUNTIME_VERSION}
      tilo-install looks there and nowhere else that exists during a container
      build, so every packaged app is refused at install time with
      RuntimeDependencyMissing and the image never builds."
  fi
  if containerfile_has_copy "$CONTAINERFILE" "$sysroot/" "$sysroot/" builder; then
    ok "the image copies the runtime bundle from and to $sysroot"
  else
    bad "os/Containerfile does not copy $sysroot/ to $sysroot/ — the booted system
      would have no bundle at the path the launcher resolves, and every app would
      fail at launch with RuntimeBundleMissing."
  fi
fi

# --- 7. every REQUIRED sandbox service has something serving it --------------
# `[sandbox.services]` entries marked `required@<v>` are hard launch
# dependencies: resolve_service_binds returns RequiredMissing when
# $XDG_RUNTIME_DIR/tilo/services/<name>.sock is absent, so the app does not
# start at all. An optional one only warns. Shipping an app whose required
# service has no supervisor in the image is therefore shipping an app that
# cannot run — which is how the Terminal came to declare `ush = "required@1"`
# while nothing built, installed or started a `ush`.
#
# The convention this asserts: a required service `<name>` is served by
# /usr/lib/systemd/user/tilo-<name>.service, enabled directly or through a
# shipped socket activation unit, whose ExecStart
# runs a binary the image installs on the socket the launcher looks at.
required="$(grep -rh '^[a-z_-]* *= *"required@' "${manifests[@]}" \
              | sed 's/^\([a-z_-]*\) *=.*/\1/' | sort -u)"
if [ -z "$required" ]; then
  ok "no app declares a required sandbox service"
else
  for svc in $required; do
    unit_name=$svc
    unit_rel="os/usr/lib/systemd/user/tilo-$unit_name.service"
    unit="$REPO/$unit_rel"
    users="$(grep -l "^$svc *= *\"required@" "${manifests[@]}" \
               | sed 's|.*/apps/\([^/]*\)/.*|\1|' | tr '\n' ' ')"
    # Collaboration is session state owned by the compositor, not an
    # independent daemon. Starting a second broker would race the compositor
    # for the same socket and split apps across two CRDT sessions. Prove the
    # image ships the owner and that it binds the launcher's canonical path.
    if [ "$svc" = "collab" ]; then
      native="$REPO/crates/session/shell/src/compositor/native.rs"
      if grep -Eq 'let (mut )?collab_broker.*=' "$native" \
          && grep -q 'spawn_session_collab_broker' "$native" \
          && grep -q 'launcher::services::collab_socket()' "$native"; then
        ok "the native compositor starts 'collab' on the launcher's canonical service socket"
      else
        bad "apps [$users] require 'collab', but the native compositor does not visibly start its broker on launcher::services::collab_socket()."
      fi
      containerfile_installs_output "$CONTAINERFILE" shell /usr/bin/shell \
        && ok "the image installs the compositor that owns 'collab'" \
        || bad "os/Containerfile does not install /usr/bin/shell, so the compositor-owned 'collab' broker cannot start."
      continue
    fi
    # Typed tool launches are also compositor-owned session state. Their
    # handler dispatches into the shell and pumps workspace routes on its UI
    # loop; a separate systemd daemon cannot supply that ownership.
    if [ "$svc" = "tools" ]; then
      native="$REPO/crates/session/shell/src/compositor/native.rs"
      service="$REPO/crates/session/shell/src/shell/tool_intents.rs"
      if grep -Fq 'let tool_service = match crate::shell::tool_intents::spawn()' "$native" \
          && python3 -c '
import pathlib, re, sys
source = pathlib.Path(sys.argv[1]).read_text()
# Keep the call connected to the compositor chrome handle, including when
# a local binding is shared with adjacent workspace/identity handlers.
pattern = r"if let Some\(service\) = tool_service\.as_ref\(\)\s*\{\s*(?:service\.pump_workspace_settings\(&frame_builder\.chrome_handle\(\)\)|let (\w+) = frame_builder\.chrome_handle\(\);\s*service\.pump_workspace_settings\(&\1\))"
sys.exit(0 if re.search(pattern, source) else 1)
' "$native" \
          && grep -Fq 'pub const TOOL_SERVICE: &str = "tools";' "$service" \
          && grep -Fq 'launcher::services::socket_for(TOOL_SERVICE)' "$service"; then
        ok "the native compositor starts 'tools' on the canonical socket and handles workspace routes"
      else
        bad "apps [$users] require 'tools', but its compositor-owned socket and workspace handler are not wired."
      fi
      containerfile_installs_output "$CONTAINERFILE" shell /usr/bin/shell \
        && ok "the image installs the compositor that owns 'tools'" \
        || bad "os/Containerfile does not install the compositor-owned 'tools' service."
      continue
    fi
    if [ ! -r "$unit" ]; then
      bad "apps [$users] declare '$svc' as a required service, but $unit_rel does not exist.
      The launcher refuses to start those apps when
      \$XDG_RUNTIME_DIR/tilo/services/$svc.sock is absent, so they would be
      shipped unable to launch."
      continue
    fi
    socket_rel="os/usr/lib/systemd/user/tilo-$unit_name.socket"
    socket="$REPO/$socket_rel"
    if grep -q "systemctl --global enable tilo-$unit_name.service" "$CONTAINERFILE"; then
      ok "tilo-$unit_name.service is globally enabled, so '$svc' is served for every session"
    elif [ -r "$socket" ] \
        && grep -q "systemctl --global enable tilo-$unit_name.socket" "$CONTAINERFILE" \
        && containerfile_has_copy "$CONTAINERFILE" "$socket_rel" "/usr/lib/systemd/user/tilo-$unit_name.socket" \
        && grep -Fxq "ListenStream=%t/tilo/services/$svc.sock" "$socket" \
        && grep -Fxq "Service=tilo-$unit_name.service" "$socket" \
        && grep -Eq '^WantedBy=.+\.target$' "$socket"; then
      ok "tilo-$unit_name.socket is installed and globally enabled on the canonical '$svc' socket"
    else
      bad "$unit_rel has neither a globally enabled service nor an installed, enabled
      activation socket serving '$svc' and activating tilo-$unit_name.service."
    fi
    grep -q "ExecStart=.*%t/tilo/services/$svc\.sock" "$unit" \
      && ok "tilo-$unit_name.service serves the socket path the launcher resolves" \
      || bad "$unit_rel does not serve %t/tilo/services/$svc.sock, which is where
      launcher::services::socket_for('$svc') looks."
    svc_bin="$(sed -n 's|^ExecStart=\([^ ]*\).*|\1|p' "$unit" | head -1)"
    if [ -z "$svc_bin" ]; then
      bad "$unit_rel has no ExecStart, so nothing checks that its binary ships"
    elif containerfile_copy_contract "$CONTAINERFILE" | \
         awk -F '\t' -v d="$svc_bin" '($1 == "builder" || $1 == "components.toml") && $3 == d { found=1 } END { exit !found }'; then
      ok "the image installs $svc_bin for '$svc'"
    else
      bad "os/Containerfile never copies $svc_bin into the image, so
      tilo-$unit_name.service would fail at every start and apps [$users] could not launch."
    fi
  done
fi

# --- every media feature the image turns on has to exist ---------------------
# The apps' engines sit behind cargo features that default off so that --shots,
# the offline tests and CI need no GStreamer stack, no camera and no network.
# os/Containerfile names them in APP_FEATURES; the list must match the default
# collection in dist/apps/app-collections.toml (what CI package-apps builds).
#
# A pair naming a feature no crate has is not a no-op and not a silent
# mis-build: cargo refuses the entire invocation ("none of the selected packages
# contains these features"), so the image build dies — but it dies *after* the
# builder stage's 600-odd package install and the cold Rust compile, tens of
# minutes in, at the one step whose log nobody reads to the end. That is how
# `video-editor/portal` reached this file: video-editor enables `ui`'s portal
# unconditionally and has no portal feature of its own, so the pair looked exactly
# like the twelve valid ones next to it.
#
# Checked against the workspace's own manifests rather than a restated list here,
# so the two cannot drift. They are read directly, not through `cargo metadata`:
# cargo takes the shared ~/.cargo package-cache lock even with --offline
# --no-deps, so this preflight used to wait as long as any other build on the
# machine held it. A manifest that cannot be read is a FAILURE, not a skip:
# "could not look" and "looked and found nothing wrong" are the same output
# otherwise.
feat_line="$(sed -n 's/^ *&& APP_FEATURES="\([^"]*\)".*/\1/p' "$CONTAINERFILE" | head -1)"
if [ -z "$feat_line" ]; then
  bad "could not read APP_FEATURES from os/Containerfile — the declaration it is
      parsed out of moved. Unchecked, the image would build every media app on its
      default feature set: a music player that cannot produce sound, a video player
      that synthesises frames, a camera that cannot capture."
else
  bogus="$(python3 - "$REPO" "$feat_line" <<'PY' 2>&1
import sys, tomllib
from pathlib import Path

repo = Path(sys.argv[1])
root = tomllib.loads((repo / "Cargo.toml").read_text())
members = []
for pattern in root["workspace"]["members"]:
    members += sorted(repo.glob(pattern)) if any(c in pattern for c in "*?[") else [repo / pattern]

def features(manifest: dict) -> set[str]:
    # What cargo metadata reports: declared features plus the implicit feature
    # of every optional dependency no `dep:` entry hides.
    declared = manifest.get("features", {})
    hidden = {v[4:] for values in declared.values() for v in values if v.startswith("dep:")}
    tables = [manifest] + list(manifest.get("target", {}).values())
    optional = {name for table in tables
                for key in ("dependencies", "build-dependencies", "dev-dependencies")
                for name, spec in table.get(key, {}).items()
                if isinstance(spec, dict) and spec.get("optional")}
    return set(declared) | (optional - hidden)

have = {}
for member in members:
    manifest = tomllib.loads((member / "Cargo.toml").read_text())
    if "package" in manifest:
        have[manifest["package"]["name"]] = features(manifest)
if not have:
    sys.exit("the workspace manifests name no packages")
for pair in (s.strip() for s in sys.argv[2].split(",")):
    if not pair:
        continue
    if "/" not in pair:
        print(f"{pair}: not package-qualified, so cargo applies it to the whole workspace")
        continue
    pkg, feat = pair.split("/", 1)
    if pkg not in have:
        print(f"{pair}: no workspace package named {pkg}")
    elif feat not in have[pkg]:
        opts = ", ".join(sorted(have[pkg])) or "none"
        print(f"{pair}: {pkg} has no feature {feat} (it has: {opts})")
PY
)" || bogus="${bogus:-the workspace manifests could not be read}"
  if [ -z "$bogus" ]; then
    n="$(printf '%s' "$feat_line" | tr ',' '\n' | grep -c .)"
    ok "all $n APP_FEATURES pairs name a feature their package really has"
  else
    bad "os/Containerfile asks cargo for features that do not exist (or the manifests are unreadable):
$(printf '%s\n' "$bogus" | sed 's/^/      /')
      cargo refuses the whole build for any one of these, after the builder stage
      has already installed its packages and compiled the workspace."
  fi
fi

# Thin apps must use the same feature set and package exclusions as CI's default
# collection. Putting browser/wpe into the image thin graph unified ui/webview
# into every tapp and added ~1.9 MB of .text versus package-apps (#1158).
collection_features="$(python3 "$REPO/tools/apps/app_release.py" --collection default   | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin)["features"]))')"
image_features="$(printf '%s' "$feat_line" | tr ',' '\n' | sed '/^$/d' | sort -u | paste -sd, -)"
collection_sorted="$(printf '%s' "$collection_features" | tr ',' '\n' | sed '/^$/d' | sort -u | paste -sd, -)"
if [ "$image_features" = "$collection_sorted" ]; then
  ok "image APP_FEATURES match the default collection CI package-apps builds"
else
  bad "image APP_FEATURES diverge from dist/apps/app-collections.toml default collection
      image:      $image_features
      collection: $collection_sorted
      a divergent graph is what made image thin apps ~1.9 MB of .text larger than CI"
fi
if grep -Eq 'THIN_EXCLUDES="browser"' "$CONTAINERFILE" \
   && ! grep -Fq 'THIN_BROWSER_FEATURE=",browser/wpe"' "$CONTAINERFILE" \
   && ! grep -Fq 'APP_FEATURES="${APP_FEATURES}${THIN_BROWSER_FEATURE}"' "$CONTAINERFILE"; then
  ok "image thin apps always exclude browser (separate WPE runtime), matching the collection"
else
  bad "image thin-apps step can still pull browser/wpe into the unified collection graph
      which reintroduces the ~1.9 MB .text gap versus CI (#1158)"
fi

# --- nobody may re-enable ashpd's default backend ----------------------------
# ashpd refuses to compile with both of its runtime backends on:
#   compile_error!("You can't enable both async-std & tokio features at once")
# Its own default is async-std; the workspace pins it to
# `default-features = false, features = ["tokio"]` so the whole graph agrees on
# one runtime. A member that says `default-features = true` does not get a
# private async-std copy — cargo unions features, so it turns ashpd off for
# every portal-using crate at once.
#
# apps/chat did exactly that, and it was invisible: chat's ashpd is behind
# `live-media`, which is off by default, so no ordinary build, no --shots run
# and no CI job ever compiled it. The OS image is the first thing that asks for
# that feature, so the image build is where it surfaced — after the cold Rust
# compile. A grep costs nothing and catches it before the build starts.
offenders="$(grep -rln 'ashpd *= *{[^}]*default-features *= *true' \
               --include=Cargo.toml "$REPO" 2>/dev/null \
             | grep -v '/vendor/' | sed "s|^$REPO/||" | tr '\n' ' ')"
if [ -z "$offenders" ]; then
  ok "no crate re-enables ashpd's default (async-std) backend over the workspace pin"
else
  bad "these crates ask for ashpd with default-features = true: $offenders
      ashpd's default is async-std and the workspace pins tokio. Cargo unions
      features, so this does not stay local — ashpd then has both backends and
      refuses to compile at all, for every crate that uses a portal."
fi

exit "$fail"
