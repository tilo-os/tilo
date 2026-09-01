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
#   * Every apps/<name>/qos-app.toml declares `runtime = "qos-runtime/<v>"`.
#     The launcher resolves that literal string to /usr/lib/qos-runtime/<v>/
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
PACK="$REPO/os/scripts/pack-apps.sh"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for f in "$CONTAINERFILE" "$PACK" "$REPO/Cargo.toml"; do
  [ -r "$f" ] || { bad "missing $f"; exit 1; }
done

manifests=("$REPO"/apps/*/qos-app.toml)
[ -e "${manifests[0]}" ] || { bad "no apps/*/qos-app.toml found"; exit 1; }

# --- 1. the baked runtime version is the one the apps ask for ----------------
baked="$(sed -n 's/^ARG RUNTIME_VERSION=\(.*\)$/\1/p' "$CONTAINERFILE" | head -1)"
if [ -z "$baked" ]; then
  bad "could not read ARG RUNTIME_VERSION — the declaration it is parsed out of moved"
else
  declared="$(sed -n 's/^runtime  *= *"qos-runtime\/\([^"]*\)".*/\1/p' "${manifests[@]}" \
                | sort -u)"
  count="$(printf '%s\n' "$declared" | grep -c . || true)"
  if [ "$count" -ne 1 ]; then
    bad "apps declare $count different runtime versions, so no single bake can serve them:
      $(printf '%s' "$declared" | tr '\n' ' ')"
  elif [ "$declared" = "$baked" ]; then
    ok "runtime bundle bakes $baked, which is what every app manifest declares"
  else
    bad "os/Containerfile bakes qos-runtime/$baked but the apps declare qos-runtime/$declared
      The launcher looks for /usr/lib/qos-runtime/$declared/ and will refuse every
      preinstalled app with RuntimeBundleMissing. Change ARG RUNTIME_VERSION in
      os/Containerfile, or the manifests, so the two agree."
  fi
fi

# --- 2. every app directory is a workspace member cargo can build by name ----
missing_member=0
bad_name=0
for m in "${manifests[@]}"; do
  dir="$(dirname "$m")"
  rel="apps/$(basename "$dir")"
  grep -qF "\"$rel\"" "$REPO/Cargo.toml" || { bad "$rel is not a workspace member"; missing_member=1; }
  # The Containerfile passes the directory name to `cargo build -p`, so the
  # package name has to equal it.
  pkg="$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)"
  if [ "$pkg" != "$(basename "$dir")" ]; then
    bad "$rel has package name '$pkg'; os/Containerfile builds it as '-p $(basename "$dir")'"
    bad_name=1
  fi
done
[ "$missing_member" = 1 ] || ok "all ${#manifests[@]} app manifests sit in workspace members"
[ "$bad_name" = 1 ] || ok "every app package name matches its directory, so -p resolves"

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
  printf '%s\n' "$have" | grep -qx "$want" || {
    bad "$rel: manifest entrypoint is '$entry' but the crate builds [$(printf '%s' "$have" | tr '\n' ' ')]"
    no_bin=1
  }
done
[ "$no_bin" = 1 ] || ok "every manifest entrypoint names a binary its crate builds"

# Thin qpkg binaries must all opt into the same shared-runtime edge. Merely
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

if grep -q 'MAX_QPKG_BYTES="${MAX_QPKG_BYTES:-10000000}"' "$PACK"; then
  ok "pack-apps refuses qpkgs at or above 10 MB"
else
  bad "pack-apps no longer enforces the strict 10 MB qpkg ceiling"
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
elif grep -q "^COPY --from=builder $store/ $store/\$" "$CONTAINERFILE"; then
  ok "the image copies the app store from and to $store, so registry paths resolve"
else
  bad "os/Containerfile does not copy $store/ to $store/
      pack-apps.sh records absolute payload paths; copying the store anywhere
      else leaves every registry row pointing at a directory that is not there."
fi

# --- 5. the launcher actually reads that store -------------------------------
reg="$REPO/crates/packaging/launcher/src/registry.rs"
if [ ! -r "$reg" ]; then
  bad "missing $reg"
elif grep -q "SYSTEM_DATA_DIR: &str = \"$store\"" "$reg"; then
  ok "the launcher's system store constant is $store"
else
  bad "$reg does not point SYSTEM_DATA_DIR at $store — the image would ship a
      store nothing reads."
fi

# --- 6. the runtime bundle is baked where qos-install looks for it ------------
# pack-apps.sh installs each package with `qos-install install`, and that refuses
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
sysroot="$(sed -n 's|^ *PathBuf::from("\(/usr/lib/qos-runtime\)").join(version)$|\1|p' \
             "$BUNDLE_LIB" | head -1)"
if [ ! -r "$BUNDLE_LIB" ] || [ ! -r "$INSTALL_MAIN" ]; then
  bad "missing $BUNDLE_LIB or $INSTALL_MAIN — cannot check where the bundle is looked up"
elif [ -z "$sysroot" ]; then
  bad "could not read the system bundle root out of runtime_bundle::system_bundle_path
      ($BUNDLE_LIB) — the declaration it is parsed out of moved, so this check
      cannot see its subject and must not pass."
else
  if grep -q 'system_bundle_path' "$INSTALL_MAIN"; then
    ok "qos-install resolves a declared runtime against $sysroot"
  else
    bad "$INSTALL_MAIN no longer consults system_bundle_path, so baking into
        $sysroot would no longer satisfy the install-time runtime check."
  fi
  if grep -q -- "--out \"$sysroot/\${RUNTIME_VERSION}\"" "$CONTAINERFILE"; then
    ok "os/Containerfile bakes the runtime bundle into $sysroot/\${RUNTIME_VERSION}"
  else
    bad "os/Containerfile does not bake the runtime bundle into $sysroot/\${RUNTIME_VERSION}
      qos-install looks there and nowhere else that exists during a container
      build, so every packaged app is refused at install time with
      RuntimeDependencyMissing and the image never builds."
  fi
  if grep -q "^COPY --from=builder $sysroot/ $sysroot/\$" "$CONTAINERFILE"; then
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
# $XDG_RUNTIME_DIR/qos/services/<name>.sock is absent, so the app does not
# start at all. An optional one only warns. Shipping an app whose required
# service has no supervisor in the image is therefore shipping an app that
# cannot run — which is how the Terminal came to declare `ush = "required@1"`
# while nothing built, installed or started a `ush`.
#
# The convention this asserts: a required service `<name>` is served by
# /usr/lib/systemd/user/qos-<name>.service, globally enabled, whose ExecStart
# runs a binary the image installs on the socket the launcher looks at.
required="$(grep -rh '^[a-z_-]* *= *"required@' "${manifests[@]}" \
              | sed 's/^\([a-z_-]*\) *=.*/\1/' | sort -u)"
if [ -z "$required" ]; then
  ok "no app declares a required sandbox service"
else
  for svc in $required; do
    unit_rel="os/usr/lib/systemd/user/qos-$svc.service"
    unit="$REPO/$unit_rel"
    users="$(grep -l "^$svc *= *\"required@" "${manifests[@]}" \
               | sed 's|.*/apps/\([^/]*\)/.*|\1|' | tr '\n' ' ')"
    if [ ! -r "$unit" ]; then
      bad "apps [$users] declare '$svc' as a required service, but $unit_rel does not exist.
      The launcher refuses to start those apps when
      \$XDG_RUNTIME_DIR/qos/services/$svc.sock is absent, so they would be
      shipped unable to launch."
      continue
    fi
    grep -q "systemctl --global enable qos-$svc.service" "$CONTAINERFILE" \
      && ok "qos-$svc.service is globally enabled, so '$svc' is served for every session" \
      || bad "$unit_rel exists but os/Containerfile never runs
      'systemctl --global enable qos-$svc.service' — the unit would ship disabled
      and apps [$users] still could not launch."
    grep -q "ExecStart=.*%t/qos/services/$svc\.sock" "$unit" \
      && ok "qos-$svc.service serves the socket path the launcher resolves" \
      || bad "$unit_rel does not serve %t/qos/services/$svc.sock, which is where
      launcher::services::socket_for('$svc') looks."
    svc_bin="$(sed -n 's|^ExecStart=\([^ ]*\).*|\1|p' "$unit" | head -1)"
    if [ -z "$svc_bin" ]; then
      bad "$unit_rel has no ExecStart, so nothing checks that its binary ships"
    elif grep -q "^COPY --from=builder [^ ]* $svc_bin\$" "$CONTAINERFILE"; then
      ok "the image installs $svc_bin for '$svc'"
    else
      bad "os/Containerfile never copies $svc_bin into the image, so
      qos-$svc.service would fail at every start and apps [$users] could not launch."
    fi
  done
fi

# --- every media feature the image turns on has to exist ---------------------
# The apps' engines sit behind cargo features that default off so that --shots,
# the offline tests and CI need no GStreamer stack, no camera and no network.
# os/Containerfile names them in APP_FEATURES so the image, and only the image,
# compiles them in.
#
# A pair naming a feature no crate has is not a no-op and not a silent
# mis-build: cargo refuses the entire invocation ("none of the selected packages
# contains these features"), so the image build dies — but it dies *after* the
# builder stage's 600-odd package install and the cold Rust compile, tens of
# minutes in, at the one step whose log nobody reads to the end. That is how
# `video_editor/portal` reached this file: video_editor enables `ui`'s portal
# unconditionally and has no portal feature of its own, so the pair looked exactly
# like the twelve valid ones next to it.
#
# Checked against cargo metadata rather than a restated list here, so the two
# cannot drift. Metadata that cannot be read is a FAILURE, not a skip: "could not
# look" and "looked and found nothing wrong" are the same output otherwise.
feat_line="$(sed -n 's/^ *&& APP_FEATURES="\([^"]*\)".*/\1/p' "$CONTAINERFILE" | head -1)"
if [ -z "$feat_line" ]; then
  bad "could not read APP_FEATURES from os/Containerfile — the declaration it is
      parsed out of moved. Unchecked, the image would build every media app on its
      default feature set: a music player that cannot produce sound, a video player
      that synthesises frames, a camera that cannot capture."
else
  meta="$(cd "$REPO" && cargo metadata --no-deps --format-version 1 --offline 2>/dev/null)"
  if [ -z "$meta" ]; then
    bad "cargo metadata produced nothing, so the APP_FEATURES pairs in os/Containerfile
      could not be checked. Treated as a failure: an unknown feature name stops the
      image build only after the cold Rust compile."
  else
    bogus="$(printf '%s' "$meta" | python3 -c '
import json, sys
have = {p["name"]: set(p["features"]) for p in json.load(sys.stdin)["packages"]}
for pair in (s.strip() for s in sys.argv[1].split(",")):
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
' "$feat_line" 2>&1)"
    if [ -z "$bogus" ]; then
      n="$(printf '%s' "$feat_line" | tr ',' '\n' | grep -c .)"
      ok "all $n APP_FEATURES pairs name a feature their package really has"
    else
      bad "os/Containerfile asks cargo for features that do not exist:
$(printf '%s\n' "$bogus" | sed 's/^/      /')
      cargo refuses the whole build for any one of these, after the builder stage
      has already installed its packages and compiled the workspace."
    fi
  fi
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
