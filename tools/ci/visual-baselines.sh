#!/usr/bin/env bash
# Render the visual goldens anew on the project's builder image (t.toml's
# `image`) and bundle the candidates for review, with that image recorded in
# .config/visual-goldens-image and t.toml regenerated to match.
#
#   t outputs visual-baselines   # renders, then downloads target/visual-baseline-review
#   git apply target/visual-baseline-review/candidates.patch
#
# Runs as t's `visual-baselines` check, which gives it a private writable copy
# of the sources and a Git checkout of the commit, so `git diff` shows exactly
# what the render changed.
set -euo pipefail
review=${CARGO_TARGET_DIR:?}/visual-baseline-review
rm -rf "$review"
mkdir -p "$review/files"

image=$(sed -n 's/^image = "\(.*\)"$/\1/p' t.toml)
image=${image%%$'\n'*}
[[ $image == *@sha256:* ]] || { echo "t.toml names no pinned project image" >&2; exit 2; }
filter=$(python3 tools/ci/nextest_filters.py | sed -n 's/^GOLDEN_FILTER=//p')
packages=()
for name in $(grep -o 'package([A-Za-z0-9_-]*)' <<<"$filter" | sed 's/^package(\(.*\))$/\1/' | sort -u); do
    packages+=(-p "$name")
done

# Diff against the tree as given, uncommitted changes included, so the patch
# holds only what the render changed.
paths=(crates/sdk/ui-core/tests/fixtures/typography apps/ebook/tests/fixtures/typography
       crates/session/shell/tests/snapshots/e2e
       .config/visual-goldens-image t.toml)
export GIT_INDEX_FILE=$TMPDIR/visual-baselines.index
git read-tree HEAD
for path in "${paths[@]}"; do
    [[ ! -e $path ]] || git add -A -- "$path"
done

status=0
UPDATE_EBOOK_MATH_BASELINE=1 UPDATE_EBOOK_TYPOGRAPHY_BASELINE=1 UPDATE_SNAPSHOTS=1 \
UPDATE_TYPOGRAPHY_BASELINES=1 TILO_REQUIRE_GPU=1 \
    bash tools/test-env.sh cargo nextest run --locked --profile ci --tests "${packages[@]}" \
    --no-tests=pass -E "$filter" || status=$?

bash tools/ci/golden-image.sh record "$image"
python3 tools/ci/t_toml.py write
for directory in e2e; do
    python3 tools/test-assets.py crates/session/shell/tests/snapshots/artifacts.json \
        --prune-identical-candidates "crates/session/shell/tests/snapshots/$directory"
done
python3 tools/test-assets.py crates/sdk/ui-core/tests/fixtures/typography/artifacts.json \
    --prune-identical-candidates crates/sdk/ui-core/tests/fixtures/typography
python3 tools/test-assets.py apps/ebook/tests/fixtures/typography/artifacts.json \
    --prune-identical-candidates apps/ebook/tests/fixtures/typography

git ls-files --others --exclude-standard -z -- "${paths[@]}" | xargs -0 -r git add -N --
git diff --name-only --diff-filter=AM -- "${paths[@]}" > "$review/changed-files.txt"
git diff --stat -- "${paths[@]}" > "$review/diff-stat.txt"
git diff --binary -- "${paths[@]}" > "$review/candidates.patch"
while IFS= read -r path; do
    [[ -z $path ]] || cp --parents "$path" "$review/files"
done < "$review/changed-files.txt"
[[ -s $review/changed-files.txt ]] || echo 'No visual baseline changes were produced.' > "$review/no-changes.txt"
(cd "$review" && sha256sum candidates.patch > candidates.patch.sha256)
cat "$review/diff-stat.txt"
exit "$status"
