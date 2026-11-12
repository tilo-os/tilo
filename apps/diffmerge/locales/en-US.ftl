# Diff & Merge: source locale (en-US).
# Counts are plural-aware; variables are data, never preformatted Fluent.

## Status bar
diff-status-empty = No files open
diff-status-identical = The files are identical
diff-status-counts = { $added } added, { $removed } removed, { $modified } modified
diff-status-position = Change { $current } of { $total }
merge-status-changes =
    { $count ->
        [one] 1 change, { $resolved } resolved
       *[other] { $count } changes, { $resolved } resolved
    }
diff-status-summary =
    { $added } added, { $removed } removed, { $modified } modified, in { $blocks ->
        [one] 1 change
       *[other] { $blocks } changes
    }
diff-status-summary-at = { $added } added, { $removed } removed, { $modified } modified. Change { $current } of { $total }
merge-status-summary =
    { $count ->
        [one] 1 change
       *[other] { $count } changes
    }, { $resolved } resolved, { $conflicts ->
        [one] 1 conflict left
       *[other] { $conflicts } conflicts left
    }

## Resource labels
merge-title = Merge of { $name }
merge-title-pair = Merge of { $ours } and { $theirs }
merge-title-empty = Merged result

## Merged result
merged-title = Merged result
merged-conflicts =
    { $count ->
        [one] 1 conflict
       *[other] { $count } conflicts
    }
merged-resolved = All resolved

## Change resolution
hunk-conflict = Conflict
hunk-auto-ours = Auto-resolved: ours
hunk-auto-theirs = Auto-resolved: theirs
hunk-took-ours = Resolved: ours
hunk-took-theirs = Resolved: theirs
hunk-took-both = Resolved: both
hunk-written = Resolved: own text
take-ours = Take ours
take-theirs = Take theirs
take-both = Take both
take-ours-short = Ours
take-theirs-short = Theirs
take-both-short = Both

## Sources
source-base = Base
source-ours = Ours
source-theirs = Theirs
source-left = Left
source-right = Right

## Empty and error states
empty-title = Open two files to compare them
empty-message = Choose two text files with Open files, or load a sample to see how a comparison reads.
merge-empty-title = No merge open
merge-empty-message = A three-way merge starts from a common base and two edited versions of it. Load a sample to try one.
error-title = Couldn’t open the files

## Saving the merged result
save-title = Save the merged result
save-accept = Save
save-saved = Saved to { $name }
save-failed = Could not save: { $reason }
save-cancelled = The merged result was not saved

## Comparing folders
folder-choose-left = Choose the left folder
folder-choose-right = Choose the right folder
folder-choose-accept = Choose
folder-cancelled = Choosing folders was cancelled
folder-failed = Could not compare the folders: { $reason }
folder-summary = { $left } and { $right }: { $added ->
    [one] 1 file only on the left
   *[other] { $added } files only on the left
}, { $removed ->
    [one] 1 file only on the right
   *[other] { $removed } files only on the right
}, { $modified ->
    [one] 1 changed file
   *[other] { $modified } changed files
}
folder-truncated = Only the first part of the folders was compared
folder-status-counts = { $added } added, { $removed } removed, { $modified } modified
folder-meta-only-here = only here, { $size }
folder-meta-only-other = only there, { $size }
folder-meta-size = { $left } became { $right }
folder-meta-contents = different contents
source-absent = (no such file)

## Toolbar
toolbar-diff = Diff
toolbar-merge = 3-way merge
toolbar-merge-short = Merge
toolbar-previous = Previous change
toolbar-next = Next change
toolbar-open = Open files
toolbar-sample = Load sample
toolbar-sample-short = Sample
toolbar-save = Save merged result
toolbar-save-short = Save
toolbar-saved = Save again
toolbar-saved-short = Save
toolbar-save-as = Save as…
toolbar-folders = Folder list
toolbar-compare-folders = Compare folders…
toolbar-compare-folders-short = Folders

## History
history-title = Merged result
history-originals = Original inputs are unavailable: base, ours and theirs were not recorded. Recorded choices are metadata only; historical change identity cannot be verified.
history-session = Session label: { $label }
history-choice = Recorded change { $change }: { $choice }
history-output = Merged result
history-none = No merged result was recorded in this version
history-page-output = Merged result
history-page-originals = Original inputs unavailable and recorded choices

## Opposite-only aligned source lines

diff-gap-added-right = { $count ->
    [one] 1 line added on the right
   *[other] { $count } lines added on the right
}
diff-gap-removed-left = { $count ->
    [one] 1 line removed on the left
   *[other] { $count } lines removed on the left
}

## History: what each change did, by field and by verb
diffmerge-session-merged = Edited the merged result
diffmerge-open-comparison = Compared { $left_label } with { $right_label }
diffmerge-open-merge = Opened a merge of { $ours_label } and { $theirs_label }
diffmerge-take-ours = Took ours for a change
diffmerge-take-theirs = Took theirs for a change
diffmerge-take-both = Took both sides for a change
diffmerge-resolve = Resolved a change by hand

## Accessibility
diff-a11y-open-two-files = Open these two files as a comparison
diff-a11y-no-files = No files loaded
diff-a11y-start-comparison = Use Open files to start a local comparison.
diff-a11y-status = Comparison status
diff-a11y-window = Diff and Merge
diff-a11y-window-description = Compare local files or resolve a shared three-way merge
diff-a11y-switch-diff-source = Switch the visible local diff source
diff-a11y-show-change = Show this local change block
diff-a11y-switch-merge-source = Switch the visible merge source
diff-a11y-resolve-hunk = Resolve this shared merge hunk
diff-a11y-merge-group = Three-way merge
diff-a11y-one-source = One source is visible at a time. Use Left and Right Arrow to switch source.
diff-a11y-side-by-side = Base, ours, and theirs are shown side by side.

diff-a11y-folders-truncated = Only the first part of the folders was compared
diff-a11y-choose-file = Choose a file to compare it

## Facts, property labels and section titles
diffmerge-a11y-change-lines = Change { $change }: lines { $first } through { $last }
diffmerge-a11y-show-file = Show { $source } file
diffmerge-a11y-changes = { $changes ->
    [one] One change. Scroll to review the aligned source lines.
   *[other] { $changes } changes. Scroll to review the aligned source lines.
}
diffmerge-a11y-between = Diff between { $left } and { $right }
diffmerge-a11y-for-change = { $choice } for change { $change }
diffmerge-a11y-show-source = Show { $source } source

## Actions

diffmerge-effects-open-files = Open files
diffmerge-effects-open-one = Open one file
diffmerge-effects-save-merged = Save merged result
diffmerge-effects-save-merged-as = Save merged result as…
diffmerge-effects-compare-folders = Compare folders
diffmerge-effects-load-sample = Load sample
diffmerge-effects-show-diff = Two-way diff
diffmerge-effects-show-merge = Three-way merge
diffmerge-effects-previous-change = Previous change
diffmerge-effects-next-change = Next change
diffmerge-effects-show = Show change
diffmerge-session = Diff & Merge
diffmerge-session-open-comparison = Compare two texts
diffmerge-session-open-merge = Merge three texts
diffmerge-session-take-ours = Take ours
diffmerge-session-take-theirs = Take theirs
diffmerge-session-take-both = Take both
diffmerge-session-resolve = Resolve with custom text
