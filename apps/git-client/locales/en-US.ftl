git-remote-running = Working with remote…
git-remote-busy = Wait for the current remote operation to finish.
git-remote-worker-stopped = The remote worker stopped unexpectedly. Check repository state before retrying.

# Screens
git-screen-changes = Changes
git-screen-history = History
git-screen-branches = Branches
git-app-description = Repository changes, history, branches and remotes
git-view = View

# Window bar
git-diff-unified-name = Unified diff
git-diff-split-name = Side-by-side diff
git-syntax-name = Syntax highlighting
git-dismiss-error = Dismiss error

git-switched = Switched to { $name }
git-switched-stashed = Switched to { $name }; the changes are stashed as { $stash }

# Confirmation
git-confirm-discard-file-title = Discard changes to { $name }?
git-confirm-discard-hunk-title = Discard this change?
git-confirm-delete-branch-title = Delete { $name }?
git-confirm-discard-untracked-body = The file is deleted from your working tree. This can't be undone.
git-confirm-discard-body = Your edits are lost. This can't be undone.
git-confirm-delete-branch-body = Commits that only this branch holds are lost.
git-confirm-discard = Discard
git-confirm-switch-title = Switch to { $name }?
git-confirm-switch-body = You have uncommitted changes. Park them on the stash and switch, or take them with you; a change that would be overwritten refuses either way.
git-switch-stash = Stash and switch
git-switch-carry = Keep my changes
git-cancel = Cancel

# Changes
git-section-staged = Staged
git-section-unstaged = Not staged
git-section-untracked = Untracked
git-stage-all = Stage all
git-unstage-all = Unstage all
git-stage-all-name = Stage all changes
git-unstage-all-name = Unstage all changes
git-stage = Stage
git-unstage = Unstage
git-stage-path = Stage { $path }
git-unstage-path = Unstage { $path }
git-change-open = Open { $path } diff, { $kind ->
    [added] added
    [modified] modified
    [deleted] deleted
    [renamed] renamed
    [untracked] untracked
   *[conflicted] conflicted
}
git-commit = Commit
git-commit-to = Commit to { $branch }
git-commit-message = Commit message
git-no-changes = No changes to commit
git-tree-clean = Your working tree is clean
git-select-file = Select a file to see its changes

# Diff
git-stage-file = Stage file
git-unstage-file = Unstage file
git-discard-file = Discard file
git-discard-file-name = Discard file changes
git-stage-hunk = Stage hunk
git-unstage-hunk = Unstage hunk
git-discard-hunk = Discard hunk
git-renamed-from = Renamed from { $path }
git-binary-file = Binary file
git-binary-file-detail = There is no text to compare.
git-mode-only = No changes to show
git-mode-only-detail = Only the file's mode changed.

# History
git-no-commits = No commits yet
git-commit-row-name = { $summary } by { $author }, { $age }
git-fact-commit = Commit
git-fact-date = Date
git-fact-parents = { $count ->
    [one] Parent
   *[other] Parents
}
git-date = { $day } { $month ->
    [1] January
    [2] February
    [3] March
    [4] April
    [5] May
    [6] June
    [7] July
    [8] August
    [9] September
    [10] October
    [11] November
   *[12] December
} { $year }, { $time }
git-more-refs = +{ $count }
git-fact-refs = Branches and tags
git-changed-files = Changed files

# Branches
git-section-branches = Branches
git-section-remote-branches = Remote branches
git-section-tags = Tags
git-section-stashes = Stashes
git-section-remotes = Remotes
git-fetch-name = Fetch from remote
git-pull-name = Pull from remote
git-push-name = Push to remote
git-check-out = Check out
git-check-out-name = Check out { $name }
git-delete-branch = Delete branch
git-delete-name = Delete { $name }
git-apply-stash = Apply stash
git-apply-stash-name = Apply stash { $index }
git-create-branch = Create branch
git-branch-name = Branch name
git-new-branch-name = New branch name
git-tracking-none = { $current ->
    [yes] Checked out, no upstream branch
   *[no] No upstream branch
}
git-tracking-even = { $current ->
    [yes] Checked out, up to date with { $upstream }
   *[no] Up to date with { $upstream }
}
git-tracking-ahead = { $current ->
    [yes] Checked out, { $ahead } ahead of { $upstream }
   *[no] { $ahead } ahead of { $upstream }
}
git-tracking-behind = { $current ->
    [yes] Checked out, { $behind } behind { $upstream }
   *[no] { $behind } behind { $upstream }
}
git-tracking-diverged = { $current ->
    [yes] Checked out, { $ahead } ahead, { $behind } behind { $upstream }
   *[no] { $ahead } ahead, { $behind } behind { $upstream }
}

# Open a repository
git-open-title = Open a repository
git-open-hint = Enter the path of a git repository on this computer.
git-open = Open
git-choose = Choose…
git-choose-name = Choose a repository folder
git-choose-title = Choose a repository
git-open-name = Open repository
git-repository-path = Repository path

git-collection-repository = Repository

git-section-worktrees = Working directories
git-create-worktree = Create worktree
git-worktree-branch = New worktree branch
git-worktree-path = Absolute destination path
git-worktree-start = Start point (branch, tag or commit)
git-worktree-host-required = Worktree creation requires a live repository.
git-worktree-running = Creating worktree…
git-worktree-read-failed = Could not read working directories
git-worktree-read-error = Could not read working directories: { $error }

## History of the record

git-client-git-client-repository = Opened the repository { $new }
git-client-record-repository = Recorded the repository's branches and history

# Conflicts and a commit's changed files
git-conflict-resolved = Kept { $side } for { $file }
git-conflict-ours = our side
git-conflict-theirs = their side
git-conflict-take-ours = Take ours
git-conflict-take-theirs = Take theirs
git-conflict-explainer = This file is conflicted.
# The choice above a conflicted file, costing each side: $ours is how many
# lines only your side has, $theirs how many only theirs does.
git-conflict-cost = Both sides changed this file. Keeping ours drops { $theirs ->
    [one] one line of theirs
   *[other] { $theirs } lines of theirs
}; keeping theirs drops { $ours ->
    [one] one of your lines
   *[other] { $ours } lines of yours
}.
git-back-to-commit = Back to commit
git-commit-file-heading = { $file } in { $commit }
git-commit-file-name = { $file } ({ $kind })
git-show-commit-file = Show this file's changes in the commit

# Diff controls in the window bar
git-whitespace-name = Show whitespace
git-wrap-name = Wrap long lines
