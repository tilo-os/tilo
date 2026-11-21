# shellcheck shell=bash
# Sourced by os/image/build-layer.sh for the one layer that installs the compiled tree (the
# image's, `component-files` in os/image/layers.toml), apart from that script, which every OS
# action runs: an edit here keys only the actions that install components.

# The component-files stage (os/image/helpers/component-files.Containerfile), made here rather
# than built: os/components.toml's installer arranges the compiled tree under DIR/image/<layer>/,
# which the binaries piece copies one layer at a time (`COPY --from=component-files`), given
# DIR as a build context. As a stage it wrote every binary into the stage, committed that
# (about as long again) and copied it into the image after; here it is written once onto the
# step's own disk, then copied. Every file is dated by the build's clock, as the stage's
# build context dates the files it copies: a compile of the same bytes gives the same layers.
install_components() {
  local dir=$scratch/component-files boot_from=()
  # A compile that leaves the initramfs's greeter to its own check (os-compile-boot-ARCH,
  # TILO_BOOT_GREETER=apart) is given it beside the tree.
  if [[ ! -e $TILO_COMPILED/tilo-greeter-boot && -e $os/boot/tilo-greeter-boot ]]; then
    boot_from=(--from "$os/boot")
  fi
  rm -rf "$dir"
  mkdir -p "$dir"
  (umask 022
    TILO_GITHUB_ASSET_CACHE=$repo_root/os/external-assets/cache python3 -I -B os/scripts/install-components.py \
      --manifest os/components.toml --from "$TILO_COMPILED" "${boot_from[@]}" --dest "$dir/image" --by-layer --all --quiet)
  python3 -I - "$SOURCE_DATE_EPOCH" "$dir" <<'PY'
import os, sys
clock, top = int(sys.argv[1]), sys.argv[2]
for root, dirs, files in os.walk(top):
    for entry in dirs + files:
        os.utime(os.path.join(root, entry), (clock, clock), follow_symlinks=False)
os.utime(top, (clock, clock))
PY
  layer_args+=(--build-context "component-files=$dir")
}
