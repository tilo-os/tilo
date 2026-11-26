# Sourced by the hooks whose checks are Python: puts a Python 3.11 or newer (tomllib) first on
# PATH as `python3`, or stops the hook saying how to get one. A machine may have no system
# python3 at all (a build server's minimal image), or an older one first on PATH (macOS's 3.9),
# so every hook must not need setup. In order: the python3 already on PATH; a newer one under a
# versioned name or Homebrew's; uv's managed Python, which uv finds or downloads itself.
tilo_hook_python() {
  python3 -I -c 'import tomllib' 2>/dev/null && return 0
  local py found dir
  for py in python3.14 python3.13 python3.12 python3.11 /opt/homebrew/bin/python3 /usr/local/bin/python3; do
    found=$(command -v "$py" 2>/dev/null) || continue
    dir=$(dirname "$found")
    if [ -x "$dir/python3" ] && "$dir/python3" -I -c 'import tomllib' 2>/dev/null; then
      PATH="$dir:$PATH"
      export PATH
      return 0
    fi
  done
  local uv
  uv=$(command -v uv 2>/dev/null || true)
  for found in "$HOME/.local/bin/uv" "$HOME/.cargo/bin/uv"; do
    [ -n "$uv" ] && break
    [ -x "$found" ] && uv=$found
  done
  if [ -n "$uv" ]; then
    # `uv python find` names an interpreter uv manages or the system has; `uv python install`
    # downloads one where neither exists, once per machine.
    found=$("$uv" python find '>=3.11' 2>/dev/null) || {
      "$uv" python install --quiet 3.12 >&2 && found=$("$uv" python find '>=3.11' 2>/dev/null)
    } || found=
    if [ -n "$found" ]; then
      dir=$(dirname "$found")
      if [ -x "$dir/python3" ] && "$dir/python3" -I -c 'import tomllib' 2>/dev/null; then
        PATH="$dir:$PATH"
        export PATH
        return 0
      fi
    fi
  fi
  echo "git hook: these checks need Python 3.11 or newer as python3; install it, or install uv (https://docs.astral.sh/uv/) and the hooks use its Python" >&2
  return 1
}
tilo_hook_python || exit 1
