## Actions

tasks-effect-new-project = Start a project
tasks-effect-open-issue = Open issue
tasks-effect-show-list = Show the list
tasks-effect-show-board = Show the board
tasks-effect-show-thread = Show the thread
tasks-tracker = Tasks
# What one of the issues a tracker makes is, for a line that says what is being
# made: the article is part of the noun, so a language whose article agrees with
# it writes its own.
tasks-tracker-issues-noun = { $count ->
    [one] an issue
   *[other] issues
}
tasks-tracker-create-area = New project or area
tasks-tracker-rename-area = Rename project or area
tasks-tracker-set-kind = Set project kind
tasks-tracker-involve = Add someone to a project
tasks-tracker-remove-person = Remove someone from a project
tasks-tracker-delete-area = Delete project or area

## What the tracker refuses, and what search shows.

tasks-project-gone = This project no longer exists
tasks-project-exists = That project already exists
tasks-name-one-line = A project's name fits on a line
tasks-name-project-or-area = Name the project or area, by its id or its name
tasks-project-needs-name = A project has a name
tasks-address-too-long = That address is too long
tasks-folder-unnamed = Name the folder its material is kept in
tasks-folder-unpath = That is no folder's path
tasks-folder-path-long = That folder's path is too long
tasks-issue-needs-title = An issue has a title
tasks-issue-title-one-line = An issue's title fits on a line
tasks-label-needs-name = A label has a name
tasks-goal-needs-resource = A goal is a resource of the workspace
tasks-priority-range = A priority is 1 (most urgent) to 4
tasks-say-few-words = Say { $what } in a few words
tasks-fits-chars = { $what } fits in { $max } characters
tasks-wait-limit = A wait names at most { $max } things
tasks-label-limit = An issue carries at most { $max } labels
tasks-estimate-limit = An estimate is at most { $max } points
tasks-assignee-forms = An assignee is a person's root key, agent:<id>, or outside:<name> for someone not in the workspace yet
tasks-facet-status = Status
tasks-facet-label = Label
tasks-status-backlog = Backlog
tasks-status-todo = Todo
tasks-status-in-progress = In progress
tasks-status-in-review = In review
tasks-status-done = Done
tasks-status-cancelled = Cancelled
tasks-areas-none = none yet
tasks-areas-more = { $shown } and { $more } more
tasks-areas-not-found = No project or area named { $asked }; projects and areas: { $holds }
tasks-areas-ambiguous = More than one project or area is named { $asked }; name it by its id: { $ids }
