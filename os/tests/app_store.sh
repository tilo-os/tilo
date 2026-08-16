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
  if grep -q "^ *--out \"$sysroot/\${RUNTIME_VERSION}\" \\\\\?\$" "$CONTAINERFILE"; then
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

exit "$fail"
