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
merge-status-conflicts =
    { $count ->
        [0] All conflicts resolved
        [one] 1 conflict remaining
       *[other] { $count } conflicts remaining
    }

## Resource labels
merge-title = Merge of { $name }
merge-title-pair = Merge of { $ours } and { $theirs }
merge-title-empty = Merged result
compare-title = { $left } and { $right }
compare-title-empty = Local comparison

## Merged result
merged-title = Merged result
merged-conflicts =
    { $count ->
        [one] 1 conflict
       *[other] { $count } conflicts
    }
merged-resolved = All resolved
merged-saved = Saved merged result (original inputs unavailable)
merged-unavailable = Original inputs are unavailable for this saved output. Load new inputs to resolve changes.

## Change resolution
hunk-conflict = Conflict
hunk-auto-ours = Auto-resolved: ours
hunk-auto-theirs = Auto-resolved: theirs
hunk-took-ours = Resolved: ours
hunk-took-theirs = Resolved: theirs
hunk-took-both = Resolved: both
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
error-title = Couldn’t open the files
error-chooser = Couldn’t open the file chooser: { $reason }

## Toolbar
toolbar-diff = Diff
toolbar-merge = 3-way merge
toolbar-merge-short = Merge
toolbar-previous = Previous change
toolbar-next = Next change
toolbar-open = Open files
toolbar-sample = Load sample
toolbar-sample-short = Sample

## History
history-title = Merged result
history-originals = Original inputs are unavailable: base, ours and theirs were not recorded. Recorded choices are metadata only; historical change identity cannot be verified.
history-session = Session label: { $label }
history-choice = Recorded change { $change }: { $choice }
history-output = Merged result
history-none = No merged result was recorded in this version
history-page-output = Merged result
history-page-originals = Original inputs unavailable and recorded choices
