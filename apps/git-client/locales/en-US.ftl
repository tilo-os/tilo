git-remote-running = Working with remote…
git-remote-busy = Wait for the current remote operation to finish.
git-remote-worker-stopped = The remote worker stopped unexpectedly. Check repository state before retrying.
git-remote-fetching = Fetching…
git-remote-pulling = Pulling…
git-remote-pushing = Pushing…
git-error-no-repository = No git repository at { $path }

# Screens
git-screen-changes = Changes
git-screen-history = History
git-screen-branches = Branches
git-app-description = Repository changes, history, branches and remotes

# Header
git-diff-unified = Unified
git-diff-split = Split
git-diff-unified-name = Unified diff
git-diff-split-name = Side-by-side diff
git-syntax = Syntax
git-syntax-name = Syntax highlighting
git-sidebar-show = Show sidebar
git-sidebar-hide = Hide sidebar
git-dismiss-error = Dismiss error

# Confirmation
git-confirm-discard-file-title = Discard changes to { $name }?
git-confirm-discard-hunk-title = Discard this change?
git-confirm-delete-branch-title = Delete { $name }?
git-confirm-discard-untracked-body = The file is deleted from your working tree. This can't be undone.
git-confirm-discard-body = Your edits are lost. This can't be undone.
git-confirm-delete-branch-body = Commits that only this branch holds are lost.
git-confirm-discard = Discard
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
git-fact-refs = Branches and tags
git-changed-files = Changed files

# Branches
git-section-branches = Branches
git-section-remote-branches = Remote branches
git-section-tags = Tags
git-section-stashes = Stashes
git-section-remotes = Remotes
git-fetch = Fetch
git-pull = Pull
git-push = Push
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

# Palette
git-action-open-diff = Open diff
git-action-open-commit = Open commit
git-action-check-out-branch = Check out branch
git-action-fetch-remote = Fetch remote
git-action-layout-split = Diff layout: side-by-side
git-action-layout-unified = Diff layout: unified
git-action-syntax-off = Turn off syntax highlighting
git-action-syntax-on = Turn on syntax highlighting
git-action-whitespace-hide = Hide whitespace
git-action-whitespace-show = Show whitespace
git-action-wrap-off = Stop wrapping long lines
git-action-wrap-on = Wrap long lines
git-action-show-changes = Show changes
git-action-show-history = Show history
git-action-show-branches = Show branches and remotes
git-action-commit = Commit staged changes
git-action-commit-description = Create a commit from the currently staged changes
git-action-stash = Stash working changes
git-action-stash-message = Stash message
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
