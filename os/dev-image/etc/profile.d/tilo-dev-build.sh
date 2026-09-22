# Dev image only: in-VM builds go through the builder toolchain's compiler
# cache, one cache shared by every worktree. Each worktree keeps its own
# target/ (never set CARGO_TARGET_DIR; see AGENTS.md).
export RUSTC_WRAPPER=/usr/bin/sccache
export SCCACHE_DIR=/var/cache/tilo-dev/sccache
export CARGO_INCREMENTAL=0
