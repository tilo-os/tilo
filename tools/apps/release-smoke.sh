#!/usr/bin/env bash
# What a Release / apps build must show before its packages may be signed, run from the
# repository root after dist/apps/package-apps.sh built every app of the default collection for
# this machine's architecture into target/thin-app and packaged them into dist/app-release
# (tools/apps/t-release.sh build). Each measurement gates on its own subject: a tapp over its
# budget must not hide the runtime's numbers, or the reverse, so every one runs and the script
# fails at the end if any did.
#
#   tools/apps/release-smoke.sh ARCH
set -uo pipefail
arch=${1:?usage: release-smoke.sh ARCH}
GITHUB_STEP_SUMMARY=${GITHUB_STEP_SUMMARY:-/dev/null}
target=${CARGO_TARGET_DIR:-target}
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# Shipped tapps within their size budget.
tapp_size() (
  set -euo pipefail
  python3 tools/perf/tapp_size_ratchet.py measure "${TILO_APP_RELEASE_OUT:-dist/app-release}" --arch "$arch" --summary "$GITHUB_STEP_SUMMARY"
)

# Shared runtime within its size budget.
runtime_size() (
  set -euo pipefail
  python3 tools/perf/runtime_size_ratchet.py measure $target/thin-app/libruntime_dylib.so --arch "$arch" --summary "$GITHUB_STEP_SUMMARY" --apps-dir $target/thin-app --library-dir "$(rustc --print target-libdir)" $(sed "s|^|--engine $target/thin-app/|" "$target/thin-app/runtime-engines")
)

# Report the runtime's loader cost.
loader_cost() (
  set -euo pipefail
  set -euo pipefail
  app=
  for name in $(python3 tools/perf/runtime_size_ratchet.py apps-without-media); do
    [[ -x $target/thin-app/$name ]] && { app=$target/thin-app/$name; break; }
  done
  [[ -n $app ]] || app=$(find $target/thin-app -maxdepth 1 -type f -executable ! -name '*.so' ! -name '*.d' | sort | head -1)
  [[ -n $app ]] || { echo "no built app to load" >&2; exit 1; }
  # Resolved first: an assignment beside LD_DEBUG would print rustc's own statistics.
  # An app need not accept --help; only reaching main matters, so its status is ignored.
  libdir=$(rustc --print target-libdir)
  export LD_LIBRARY_PATH="$target/thin-app:$libdir"
  stats=$(LD_DEBUG=statistics "$app" --help 2>&1 >/dev/null </dev/null \
    | sed -n 's/^ *[0-9]*: *//p' | grep -E 'time|relocations' || true)
  [[ -n $stats ]] || { echo "the loader printed no statistics for $app" >&2; exit 1; }
  loaded=$(LD_DEBUG=files "$app" --help 2>&1 >/dev/null </dev/null \
    | sed -n 's/^[[:space:]]*[0-9]*:[[:space:]]*file=\([^ ]*\) .*/\1/p' | sort -u | tr '\n' ' ' || true)
  printf '%s --help\n%s\nloaded at exec: %s\n' "$(basename "$app")" "$stats" "$loaded" \
    | tee -a "$GITHUB_STEP_SUMMARY"
  # Up, without a display: a headless drive that waits for a widget
  # that never comes keeps the process alive to read its memory.
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  XDG_RUNTIME_DIR=$runtime "$app" --headless-drive \
    '{"actions":[{"wait_for_named":{"target":"tilo-loader-report","timeout_ms":4000}}]}' \
    >/dev/null 2>&1 </dev/null &
  pid=$!
  sleep 2
  if [[ -r /proc/$pid/smaps_rollup ]]; then
    dirty=$(awk '/^Private_Dirty:/ { print $2 " kB" }' "/proc/$pid/smaps_rollup")
    media=$(awk '/^[0-9a-f]+-[0-9a-f]+ / { lib = ($6 ~ /lib(gst|glib|gobject|gio)/) }
                 lib && /^Private_Dirty:/ { kb += $2 } END { print kb + 0 " kB" }' "/proc/$pid/smaps")
    printf 'private dirty once up: %s, of it in GLib/GStreamer mappings: %s\n' "$dirty" "$media" \
      | tee -a "$GITHUB_STEP_SUMMARY"
  else
    echo "the app exited before its memory could be read" | tee -a "$GITHUB_STEP_SUMMARY"
  fi
  wait "$pid" || true
  rm -rf "$runtime"
)

# Every app draws its first frame against the runtime.
first_frame() (
  set -euo pipefail
  set -euo pipefail
  libdir=$(rustc --print target-libdir)
  driver=$(find /usr/share/vulkan/icd.d -name 'lvp_icd.*.json' -print -quit 2>/dev/null || true)
  [[ -z $driver ]] || export VK_DRIVER_FILES=$driver WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe
  export LD_LIBRARY_PATH="$target/thin-app:$libdir" LD_BIND_NOW=1
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  drawn=0 failed=()
  for manifest in apps/*/tilo-app.toml examples/apps/*/tilo-app.toml; do
    binary=$(sed -n 's/^bin  *= *"bin\/\([^"]*\)".*/\1/p' "$manifest" | head -1)
    [[ -x $target/thin-app/$binary ]] || continue
    grep -Eq '^kind *= *"service"' "$manifest" && continue
    shot="$runtime/$binary.png"
    if XDG_RUNTIME_DIR=$runtime timeout 180 "$target/thin-app/$binary" --shots="$shot" \
         >"$runtime/$binary.log" 2>&1 </dev/null && [[ -s $shot ]]; then
      drawn=$((drawn + 1))
    else
      tail -20 "$runtime/$binary.log" >&2 || true
      failed+=("$binary")
    fi
  done
  printf '%s apps drew their first frame against the runtime\n' "$drawn" | tee -a "$GITHUB_STEP_SUMMARY"
  rm -rf "$runtime"
  (( ${#failed[@]} == 0 )) || { echo "no first frame from: ${failed[*]}" >&2; exit 1; }
)

# A media app decodes through the lazily loaded runtime.
media() (
  set -euo pipefail
  set -euo pipefail
  if [[ ! -x $target/thin-app/video-player ]]; then
    echo "video-player is not among this run's apps; nothing to decode" | tee -a "$GITHUB_STEP_SUMMARY"
    exit 0
  fi
  clip=$(find $target/thin-app/build -path '*/video-player-*/out/h264-aac.mp4' -print -quit)
  [[ -s $clip ]] || { echo "video-player's build left no h264-aac.mp4 fixture" >&2; exit 1; }
  libdir=$(rustc --print target-libdir)
  driver=$(find /usr/share/vulkan/icd.d -name 'lvp_icd.*.json' -print -quit 2>/dev/null || true)
  [[ -z $driver ]] || export VK_DRIVER_FILES=$driver WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  shot="$runtime/video-player.png"
  status=0
  XDG_RUNTIME_DIR=$runtime LD_LIBRARY_PATH="$target/thin-app:$libdir" LD_DEBUG=files \
    timeout 300 $target/thin-app/video-player --shots="$shot" "$clip" \
    >"$runtime/out.log" 2>"$runtime/loader.log" </dev/null || status=$?
  if (( status != 0 )) || [[ ! -s $shot ]]; then
    grep -v -e 'file=' -e 'calling' "$runtime/loader.log" | tail -40 >&2 || true
    echo "video-player did not capture a decoded frame (exit $status)" >&2
    exit 1
  fi
  arrival=$(grep -Eo 'file=libgstreamer-1\.0\.so\.0 \[0\];  (needed|dynamically loaded) by [^ ]*' \
    "$runtime/loader.log" | head -1 || true)
  [[ -n $arrival ]] || { echo "the loader never loaded libgstreamer, yet a frame decoded" >&2; exit 1; }
  printf 'video-player decoded %s through the runtime (%s)\n' "$(basename "$clip")" "$arrival" \
    | tee -a "$GITHUB_STEP_SUMMARY"
  rm -rf "$runtime"
)

# Typst Editor typesets through its engine library.
typst() (
  set -euo pipefail
  set -euo pipefail
  if [[ ! -x $target/thin-app/typst-editor ]]; then
    echo "typst-editor is not among this run's apps; nothing to typeset" | tee -a "$GITHUB_STEP_SUMMARY"
    exit 0
  fi
  grep -Fxq libtilo_typst_engine.so $target/thin-app/runtime-engines \
    || { echo "build-thin-apps.sh did not list libtilo_typst_engine.so" >&2; exit 1; }
  libdir=$(rustc --print target-libdir)
  driver=$(find /usr/share/vulkan/icd.d -name 'lvp_icd.*.json' -print -quit 2>/dev/null || true)
  [[ -z $driver ]] || export VK_DRIVER_FILES=$driver WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  shot="$runtime/typst-editor.png"
  status=0
  XDG_RUNTIME_DIR=$runtime LD_LIBRARY_PATH="$target/thin-app:$libdir" LD_DEBUG=files \
    timeout 300 $target/thin-app/typst-editor --shots="$shot" \
    >"$runtime/out.log" 2>"$runtime/loader.log" </dev/null || status=$?
  if (( status != 0 )) || [[ ! -s $shot ]]; then
    grep -v -e 'file=' -e 'calling' "$runtime/loader.log" | tail -40 >&2 || true
    echo "typst-editor did not capture a typeset document (exit $status)" >&2
    exit 1
  fi
  arrival=$(grep -Eo 'file=libtilo_typst_engine\.so \[0\];  needed by [^ ]*' \
    "$runtime/loader.log" | head -1 || true)
  [[ $arrival == *typst-editor* ]] \
    || { echo "typst-editor typeset without loading libtilo_typst_engine.so itself: ${arrival:-not loaded}" >&2; exit 1; }
  printf 'typst-editor typeset its first document through %s\n' "$arrival" | tee -a "$GITHUB_STEP_SUMMARY"
  rm -rf "$runtime"
)

# Music runs Spotify playback from its engine library.
spotify() (
  set -euo pipefail
  set -euo pipefail
  if [[ ! -x $target/thin-app/music ]]; then
    echo "music is not among this run's apps; no Spotify engine to run" | tee -a "$GITHUB_STEP_SUMMARY"
    exit 0
  fi
  grep -Fxq libtilo_spotify_engine.so $target/thin-app/runtime-engines \
    || { echo "build-thin-apps.sh did not list libtilo_spotify_engine.so" >&2; exit 1; }
  libdir=$(rustc --print target-libdir)
  runtime=$(mktemp -d)
  chmod 700 "$runtime"
  status=0
  XDG_RUNTIME_DIR=$runtime LD_LIBRARY_PATH="$target/thin-app:$libdir" LD_DEBUG=files \
    timeout 120 $target/thin-app/music --probe-spotify-engine check \
    >"$runtime/out.log" 2>"$runtime/loader.log" </dev/null || status=$?
  if (( status != 0 )); then
    grep -v -e 'file=' -e 'calling' "$runtime/loader.log" | tail -40 >&2 || true
    echo "music could not run its Spotify engine (exit $status)" >&2
    exit 1
  fi
  arrival=$(grep -Eo 'file=libtilo_spotify_engine\.so \[0\];  needed by [^ ]*' \
    "$runtime/loader.log" | head -1 || true)
  [[ $arrival == *music* ]] \
    || { echo "music ran Spotify playback without loading libtilo_spotify_engine.so itself: ${arrival:-not loaded}" >&2; exit 1; }
  printf '%s\nthrough %s\n' "$(cat "$runtime/out.log")" "$arrival" | tee -a "$GITHUB_STEP_SUMMARY"
  rm -rf "$runtime"
)

# Apps run their engine libraries. Settings has no probe, to keep its tapp as
# it is, Calendar none for the accounts engine (its probe runs the time-zone
# one), and the PDF engine's apps, Imager, Browser, Store, Chat and the
# on-screen keyboard none (build-thin-apps.sh runs those engines' own
# checks): for those only the trace of the start counts (an app need not
# accept --help).
engines() (
  set -euo pipefail
  libdir=$(rustc --print target-libdir)
  failed=0
  for probe in "canvas libtilo_paint_engine.so --probe-paint-engine" \
               "music libtilo_youtube_engine.so --probe-youtube-engine" \
               "tube libtilo_youtube_engine.so --probe-youtube-engine" \
               "mail libtilo_accounts_engine.so --probe-accounts-engine" \
               "settings libtilo_accounts_engine.so" \
               "settings libtilo_settings_engine.so" \
               "calendar libtilo_timezone_engine.so --probe-timezone-engine" \
               "calendar libtilo_accounts_engine.so" \
               "clocks libtilo_timezone_engine.so --probe-timezone-engine" \
               "contacts libtilo_contact_directory_engine.so --probe-contact-directory-engine" \
               "mail libtilo_contact_directory_engine.so --probe-contact-directory-engine" \
               "mail libtilo_mail_engine.so" \
               "messages libtilo_contact_directory_engine.so --probe-contact-directory-engine" \
               "pdf-viewer libtilo_pdf_engine.so" \
               "file-explorer libtilo_pdf_engine.so" \
               "print-centre libtilo_pdf_engine.so" \
               "imager libtilo_fuse_engine.so" \
               "imager libtilo_accounts_engine.so" \
               "browser libtilo_browser_import_engine.so" \
               "browser libtilo_accounts_engine.so" \
               "store libtilo_app_host_engine.so" \
               "chat libtilo_emoji_engine.so" \
               "osk libtilo_emoji_engine.so"; do
    read -r app library option <<<"$probe"
    if [[ ! -x $target/thin-app/$app ]]; then
      echo "$app is not among this run's apps; no $library to run" | tee -a "$GITHUB_STEP_SUMMARY"
      continue
    fi
    if ! grep -Fxq "$library" "$target/thin-app/runtime-engines"; then
      echo "build-thin-apps.sh did not list $library" >&2
      failed=1
      continue
    fi
    if [[ -n $option ]]; then arguments=("$option" check); else arguments=(--help); fi
    runtime=$(mktemp -d)
    chmod 700 "$runtime"
    status=0
    XDG_RUNTIME_DIR=$runtime LD_LIBRARY_PATH="$target/thin-app:$libdir" LD_DEBUG=files \
      timeout 120 "$target/thin-app/$app" "${arguments[@]}" \
      >"$runtime/out.log" 2>"$runtime/loader.log" </dev/null || status=$?
    if [[ -n $option ]] && (( status != 0 )); then
      grep -v -e 'file=' -e 'calling' "$runtime/loader.log" | tail -40 >&2 || true
      echo "$app could not run $library (exit $status)" >&2
      failed=1
    else
      arrival=$(grep -Eo "file=${library//./\\.} \[0\];  needed by [^ ]*" \
        "$runtime/loader.log" | head -1 || true)
      if [[ $arrival == */$app && -n $option ]]; then
        printf '%s\nthrough %s\n' "$(cat "$runtime/out.log")" "$arrival" | tee -a "$GITHUB_STEP_SUMMARY"
      elif [[ $arrival == */$app ]]; then
        printf '%s starts with %s\n' "$app" "$arrival" | tee -a "$GITHUB_STEP_SUMMARY"
      else
        echo "$app ${option:-started} without loading $library itself: ${arrival:-not loaded}" >&2
        failed=1
      fi
    fi
    rm -rf "$runtime"
  done
  exit "$failed"
)

failed=()
# RELEASE_SMOKE_STEPS narrows the run to some of the steps (the size gate on main takes the two
# size budgets); a release runs them all.
for step in ${RELEASE_SMOKE_STEPS:-tapp_size runtime_size loader_cost first_frame media typst spotify engines}; do
  case $step in
    tapp_size|runtime_size|loader_cost|first_frame|media|typst|spotify|engines) ;;
    *) printf 'unknown release smoke step: %s\n' "$step" >&2; exit 2 ;;
  esac
  printf '==> %s\n' "$step"
  "$step" || failed+=("$step")
done
(( ${#failed[@]} == 0 )) || { printf 'release smoke checks failed: %s\n' "${failed[*]}" >&2; exit 1; }
