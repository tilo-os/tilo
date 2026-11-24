# Git: what the tilolang package keeps, and what is still owed

The Rust Git app and its model crates were deleted when this package landed. Every capability
the Rust app had, where it lives here, and the test that holds it:

| Capability | Here | Test |
|---|---|---|
| Open a repository by a typed path, by choosing its folder in the app's own picker, from the path a member opened | `open_repository`, `choose_repository`, `read_folder`, `open_browsed`, `close_picker`; the open page of `view changes` | `opening_a_repository_reads_it_into_the_app`; the app walk |
| The working tree: staged, not staged and untracked files with kind and line counts | `type Change`; `view changes_list`, `view change_row` | `opening_a_repository_reads_it_into_the_app` |
| Stage and unstage one file; stage all; unstage all | `stage_file`, `unstage_file`, `stage_all`, `unstage_all` | `a_file_is_staged_and_taken_back` |
| Per-hunk staging, unstaging and discarding | `stage_hunk`, `unstage_hunk`, `discard_hunk`; the hunk rows of `view change_detail` | `a_hunk_is_staged_taken_back_and_discarded` |
| Discard a file's changes (destructive) | `discard_file` | `a_file_is_discarded_whole` |
| Commit what is staged, with its message and the branch it lands on | `commit`; `view commit_box` | `a_commit_writes_what_is_staged_and_says_what_it_made` |
| A conflicted file's two sides, what each costs, and keeping one | `resolve_conflict` with the `ours`/`theirs` line counts the engine reads | `a_conflict_says_what_each_side_costs_and_is_settled` |
| Create, check out and delete a branch; a dirty switch asks whether to stash or carry, then switches | `create_branch`, `checkout_branch`, `switch_stashing`, `switch_carrying`, `cancel_switch`, `switch_to`; the switch dialog in `view changes` | `branches_are_made_checked_out_and_deleted`, `a_switch_over_uncommitted_work_asks_and_parks_it` |
| Save and apply a stash | `save_stash`, `apply_stash` | `stashes_and_working_directories_are_worked` |
| Create a linked working directory | `create_worktree` | `stashes_and_working_directories_are_worked` |
| Fetch, pull and push | `fetch`, `pull`, `push` | `remotes_are_fetched_pulled_and_pushed` |
| Diff layout (one column or side by side), syntax colouring, whitespace and wrapping, each kept for the person | `toggle_layout`, `toggle_syntax`, `toggle_whitespace`, `toggle_wrap`; `view change_code`, `view code_column` | `the_diff_preferences_are_this_persons_and_this_reports_them` |
| The repository work itself: reading, staging, committing, branches, stashes, conflicts, working directories, and transfers over HTTPS and SSH | `crates/engines/tilo-git`, answered through `crates/lang/tilolang-process/src/git_engine.rs` | the engine's lib tests and the end-to-end test |

## Owed: the History and Branches windows, and the commit graph's lines

The engine already publishes what both need: every commit's graph column and every graph edge
(row, column, parent row and column) in its answer, and a commit's changed files with their
diffs on request. The window does not yet draw them:

- `view history`, `view commit_page`, `view commit_file`, `view branches` and `view branch_row`
  take decoded records as parameters (`WireCommit`, `WireBranch`, `List<CommitFile>`). The
  runtime refuses that at run time — "ref_id: expected a resource reference, got record" — so
  opening History or Branches faults. The fix is a view design that hands records only to
  functions and passes names and places into views; a draft of it is in the session that ported
  this app.
- Drawing the graph's lines between columns needs that same design: a `ui.Drawing` per row from
  the engine's edges.

Both are tracked as follow-ups for the lead to split; the engine data they need is on main.
