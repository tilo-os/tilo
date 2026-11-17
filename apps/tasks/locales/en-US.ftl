tasks-title = Tasks
tasks-filters = Filters
tasks-views = View
tasks-place-list = Issues
tasks-place-board = Board
tasks-place-thread = Thread
tasks-location-filtered = { $place }, { $filter }

## What the sidebar narrows the list and the board to
tasks-filter-all = All issues
tasks-filter-mine = Mine
tasks-filter-todo = Todos
tasks-filter-unassigned = Nobody's
tasks-filter-agent = An agent's
tasks-new-issue = New issue
tasks-new-issue-placeholder = What needs doing
tasks-new-sub-issue-placeholder = A sub-issue of this one
tasks-empty-title = No issues yet
tasks-empty-message = Write down what needs doing above; sub-issues go under an issue on its page.
tasks-empty-filter-title = Nothing here yet
tasks-empty-filter-message = Choose another filter in the sidebar to see the rest.

tasks-status-backlog = Backlog
tasks-status-todo = Todo
tasks-status-in-progress = In progress
tasks-status-in-review = In review
tasks-status-done = Done
tasks-status-cancelled = Cancelled
tasks-row-open = Open issue
tasks-row-complete = Complete
tasks-row-reopen = Reopen
tasks-status-with-progress = { $status }, { $closed } of { $total }
tasks-progress = { $closed } of { $total } sub-issues closed
tasks-list-heading = { $status } { $count }

tasks-field-title = Title
tasks-field-status = Status
tasks-field-assignee = Assignee
tasks-assignee-nobody = Nobody
tasks-issue-add-label = Add label
tasks-issue-new-label = New label
tasks-issue-label-placeholder = Label
tasks-issue-gone-title = This issue was deleted
tasks-issue-gone-message = Undo brings it back.
tasks-sub-issues = Sub-issues
tasks-links = Links

## Projects and areas: the resource above the tasks
tasks-projects = Projects and areas
tasks-new-project = New project
tasks-create-button = Create
tasks-cancel = Cancel
tasks-project-none = No projects or areas yet.
tasks-field-kind = Kind
tasks-new-project-placeholder = What the work goes towards
tasks-kind-project = Project
tasks-kind-area = Area
tasks-kind-project-note = Tasks, documents and people, until it is done.
tasks-kind-area-note = Tasks, documents and people, for as long as it goes on.
tasks-project-people = People
tasks-project-no-people = Nobody yet.
tasks-project-involve = Add someone
tasks-project-person-placeholder = Who
tasks-project-role-placeholder = What they do
tasks-project-tasks = Tasks
tasks-project-no-tasks = Nothing works towards this yet.
tasks-project-documents = Documents
tasks-project-no-documents = No documents yet.
tasks-project-gone-title = This project was deleted
tasks-project-gone-message = Undo brings it back.
tasks-project-delete = Delete project
tasks-area-delete = Delete area
tasks-field-project-name = Name
tasks-field-person = Who
tasks-field-role = What they do
tasks-write-note = Write a note
tasks-link-note = Link a note
tasks-link-note-scope = This issue
tasks-notebook = Notebook
tasks-field-goal = Goal
tasks-fact-priority = Priority: { $priority }
tasks-fact-estimate = Estimate: { $points } points
tasks-fact-attachment = Attached: { $name }
tasks-priority-none = None
tasks-priority-urgent = Urgent
tasks-priority-high = High
tasks-priority-medium = Medium
tasks-priority-low = Low
tasks-fact-due = Due { $date }


tasks-thread = Thread
tasks-thread-last-from = { $speaker } said
tasks-thread-empty-title = Nothing said yet
tasks-thread-empty-message = Write a message to start the conversation.
tasks-thread-draft = Message
tasks-thread-placeholder = Write a message
tasks-thread-send = Send
# The same button while an agent session of the task reads the thread: the
# message goes to that session, which takes it in at its next step.
tasks-message-to-run = To the agent
tasks-message-read-by-run = Read by the agent
tasks-message-from-run = From the agent
tasks-message-lookup = Looked up
tasks-message-answer = Answered with a view
tasks-message-step = Ran
## History

tasks-create = Created { $title }
tasks-create-many = Created issues
tasks-move-under = Moved { $item }
tasks-assign = Assigned { $item }
tasks-place = Moved { $item } on the board
tasks-comment = Commented on { $item }
tasks-link = Linked { $label } to { $item }
tasks-delete = Deleted { $item }
tasks-import = Brought over earlier tasks
tasks-issue-title = Renamed { $item } to { $new }
tasks-issue-status = Moved { $item } to { $new }
tasks-issue-labels = Changed the labels of { $item }
tasks-issue-links = Changed the links of { $item }
tasks-add-label = Labelled { $item } { $label }
tasks-set-place = Said where the material of { $item } is kept
tasks-wait = Let { $item } wait
tasks-resume = Let { $item } go on
tasks-set-budget = Set the budget of { $item }
tasks-set-pieces-at-once = Said how many pieces of { $item } are in work at once
tasks-session-began = Began working on { $item }
tasks-session-ended = Ended a session on { $item }
tasks-stop = Stopped { $item }

## What the app is called in a window and in the launcher
tasks-app-title = Tasks

## The parts the issues are kept in

tasks-issues = Issues
# What one of the issues the parts keep is, for a line that says what is being
# made.
tasks-issues-issues-noun = { $count ->
    [one] an issue
   *[other] issues
}
tasks-issues-create = New issue
tasks-issues-create-many = New issues
tasks-issues-rename = Rename issue
tasks-issues-move-under = Move issue
tasks-issues-assign = Assign issue
tasks-issues-add-label = Label issue
tasks-issues-place = Place issue on the board
tasks-issues-comment = Comment
tasks-issues-link = Link issue
tasks-issues-link-note = Link a note
tasks-issues-delete = Delete issue
tasks-issues-plan = Plan issue
tasks-issues-set-project = Move issue to a project
tasks-issues-attach = Attach a file
tasks-issues-detach = Remove an attachment
tasks-issues-start = Start numbering a part
tasks-issues-set-place = Set where its material is kept
tasks-issues-wait = Let issue wait
tasks-issues-resume = Let issue go on
tasks-issues-set-budget = Set issue budget
tasks-issues-set-pieces-at-once = Work pieces at once
tasks-issues-session-began = Begin a session
tasks-issues-session-ended = End a session
tasks-issues-stop = Stop issue
