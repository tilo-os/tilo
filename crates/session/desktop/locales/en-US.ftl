# Top bar
continuity-dismiss = Dismiss
topbar-workspace-fallback = Workspace
topbar-calendar-none = No calendar connected
topbar-sharing-agent-controls = AGENT CONTROLS · { $name }
topbar-sharing-agent-viewing = AGENT VIEWING · { $name }
topbar-sharing-person-controls = CONTROLS · { $name }
topbar-sharing-person-viewing = VIEWING · { $name }
topbar-sharing-session-controls = SESSION CONTROLS · { $name }
topbar-sharing-session-viewing = SESSION VIEWING · { $name }
topbar-sharing-count-controlling = { $controllers } CONTROL · { $viewers } VIEWING
topbar-sharing-count = { $viewers } VIEWING
topbar-sharing-surface = surface { $surface }
topbar-open-overview = Open overview
topbar-previous-workspace = Previous workspace
topbar-next-workspace = Next workspace
topbar-workspace-settings = Workspace settings for { $workspace }
topbar-show-clock = Show clock
topbar-show-system-monitor = Show system monitor
topbar-show-calendar = Show calendar
topbar-open-quick-settings = Open quick settings
topbar-show-collaborators = Show collaborators
topbar-viewer-can-control = can control { $targets }
topbar-viewer-view-only = view only { $targets }
topbar-viewer-entry = { $name } ({ $kind ->
        [agent] agent
        [person] person
       *[session] unidentified session
    }): { $access }
topbar-collaborators-visible = Screen visible to { $viewers }. Show collaborators
topbar-capture-privacy = Privacy settings for active capture
topbar-control = Top bar control
topbar-editor-control = Top bar editor control
topbar-editor-remove-clock = Remove clock from top bar
topbar-editor-remove-system-monitor = Remove system monitor from top bar
topbar-editor-remove-calendar = Remove calendar from top bar
topbar-editor-add-clock = Add clock to top bar
topbar-editor-add-system-monitor = Add system monitor to top bar
topbar-editor-add-calendar = Add calendar to top bar
topbar-editor-title = Edit top bar
topbar-editor-hint = Drag to reorder · click Remove
topbar-widget-clock = Clock
topbar-widget-system-monitor = System monitor
topbar-widget-calendar = Calendar
topbar-editor-remove = Remove
topbar-editor-add-clock-chip = + Clock
topbar-editor-add-monitor-chip = + Monitor
topbar-editor-add-calendar-chip = + Calendar
topbar-invite = Invite
topbar-activity-unread = { $headline }  ·  { $count } new
topbar-clock-resume = Resume
topbar-clock-pause = Pause
topbar-clock-lap = Lap
topbar-clock-add-minute = +1 min
topbar-clock-close-stopwatch = Close floating stopwatch
topbar-clock-close-timer = Close floating timer
topbar-clock-floating-stopwatch = Floating stopwatch
topbar-clock-floating-timer = Floating timer
topbar-clock-floating-description = Drag to reposition. With a keyboard, hold Alt and use the arrow keys; hold Shift for larger steps.
topbar-clock-stopwatch = Stopwatch
topbar-clock-timer = Timer
topbar-clock-paused = Paused
topbar-clock-running = Running
topbar-clock-record-lap = Record lap
topbar-clock-add-one-minute = Add one minute
topbar-workspace-position = Workspace { $position } of { $total }
topbar-desktop-navigation = Desktop navigation
topbar-display-navigation = Display navigation

# Workspace activity
a11y-shell-workspace-activity = Workspace activity
a11y-shell-activity-everyone = Everyone
a11y-shell-activity-more-people = More people
a11y-shell-activity-more-people-description = Show more contributor filters
a11y-shell-activity-resume = Resume { $target }
a11y-shell-activity-resume-description = Open the last resource you worked on: { $target }
a11y-shell-activity-boundary-newer = Move the last action into the newer work session
a11y-shell-activity-boundary-older = Move the first action into the older work session
a11y-shell-activity-boundary-newer-description = Redraw this boundary by moving one action into the newer work session
a11y-shell-activity-boundary-older-description = Redraw this boundary by moving one action into the older work session
a11y-shell-activity-more-sessions = More work sessions
a11y-shell-activity-more-sessions-description = Show more work sessions
a11y-shell-activity-new-task = New task
a11y-shell-activity-new-task-description = Create a workspace task without requiring a model
a11y-shell-activity-create-task = Create task
a11y-shell-activity-cancel-task = Cancel task creation
a11y-shell-activity-everything = Everything
a11y-shell-activity-agent-tasks = Agent tasks
a11y-shell-activity-unclassified = Unclassified
a11y-shell-activity-more-lenses = More lenses
a11y-shell-activity-more-lenses-description = Show more history lenses
a11y-shell-activity-history-entries = History entries
a11y-shell-activity-previous-checkpoint = Previous checkpoint
a11y-shell-activity-next-checkpoint = Next checkpoint
a11y-shell-activity-pause-replay = Pause replay
a11y-shell-activity-play-replay = Play replay
a11y-shell-activity-history-scope = History scope
a11y-shell-activity-history-view = History view
a11y-shell-activity-time-range = Time range
a11y-shell-activity-person-filter = Person filter
a11y-shell-activity-brief-description = { $detail }; last changed { $when }; { $count } { $count ->
        [one] change
       *[other] changes
    } by { $authors }
a11y-shell-activity-task-unassigned = unassigned
a11y-shell-activity-task-assigned-agent = assigned to an agent
a11y-shell-activity-task-no-resource = no attached resource
a11y-shell-activity-task-route = Task execution route
a11y-shell-activity-task-composer-action = Task composer action
a11y-shell-activity-task-action = Selected task action
a11y-shell-activity-history-lens = History lens
a11y-shell-activity-undo-task-description = Undo every affected record in this agent task; later collaborator edits are kept
a11y-shell-activity-history-action = Selected history action
a11y-shell-activity-explorer-panel = Explorer panel
a11y-shell-activity-timeline-description = History entries; use arrow keys to navigate
a11y-shell-activity-checkpoints-description = Move through workspace checkpoints
a11y-shell-activity-replay-description = Play or pause the workspace replay
a11y-shell-activity-view-catch-up = A summary of outcomes since this device last viewed the workspace
a11y-shell-activity-view-tasks = Durable workspace tasks with resource links, execution routes, and audit trails
a11y-shell-activity-view-explore = The complete workspace history with filters and provenance
a11y-shell-activity-view-replay = A causal replay of workspace layout and participant activity
a11y-shell-activity-entry-author = By { $author }
a11y-shell-activity-tasks = Workspace tasks
a11y-shell-activity-selected-detail = Selected history detail
a11y-shell-activity-selected-detail-description = { $title }; { $count } { $count ->
        [one] action
       *[other] actions
    }; by { $authors }
a11y-shell-activity-agent-run-description = Instruction: { $instruction }; assigned by { $issuer }; run by { $agent }; reason: { $reason }
a11y-shell-activity-reason-not-recorded = not recorded
a11y-shell-activity-agent-step = { $app } in { $record }: { $summary }
a11y-shell-activity-agent-review = Agent task review
a11y-shell-activity-task-description = { $status }; { $route }; { $assignee }; { $count } audit events
a11y-shell-activity-task-latest = latest: { $latest }
activity-view-catch-up = Catch up
activity-view-tasks = Tasks
activity-view-explore = Explore
activity-view-replay = Replay
activity-title-catch-up = While you were away
activity-title-tasks = Task Center
activity-title-explore = Workspace activity
activity-title-replay = Replay workspace
activity-workspace-fallback = This workspace
activity-source-workspace = Workspace
activity-scope-this-workspace = This workspace
activity-scope-all-workspaces = All workspaces
activity-scope-workspace-short = Workspace
activity-scope-all-short = All
activity-scope-here = Here
activity-period-all-time = All time
activity-period-today = Today
activity-period-week = 7 days
activity-period-month = 30 days
activity-people-everyone = Everyone
activity-first-people = First people…
activity-more = More…
activity-pane-timeline = Timeline
activity-pane-browse = Browse
activity-pane-details = Details
activity-resume-heading = YOU LEFT OFF
activity-resume-context = { $app } · { $when }
activity-resume-button = Resume →
activity-pending-edits = Waiting for current edits to finish
activity-catch-up-quiet = { $workspace } · Nothing new needs your attention
activity-catch-up-one = { $workspace } · { $name } kept things moving
activity-catch-up-two = { $workspace } · { $first } and { $second } kept things moving
activity-catch-up-many = { $workspace } · { $first }, { $second } and { $others } { $others ->
        [one] other was
       *[other] others were
    } active
activity-changes-workspace = { $workspace } · { $changes } { $changes ->
        [one] change
       *[other] changes
    } by { $people } { $people ->
        [one] person
       *[other] people
    }
activity-changes-everywhere = Every workspace on this machine · { $changes } { $changes ->
        [one] change
       *[other] changes
    } by { $people } { $people ->
        [one] person
       *[other] people
    }
activity-coverage-excluded = History unavailable for { $apps }
activity-coverage-missing = { $count } { $count ->
        [one] record
       *[other] records
    } unavailable
activity-coverage-capped = { $count } additional { $count ->
        [one] record
       *[other] records
    } truncated
activity-replay-checkpoints-unavailable = { $count } { $count ->
        [one] checkpoint
       *[other] checkpoints
    } unavailable
activity-latest = Latest · { $headline }
activity-history-pending = History is waiting for current edits to finish
activity-caught-up = You're caught up
activity-summary-on-device = On-device summary
activity-summary-glance = At a glance
activity-more-sessions = More work sessions…
activity-first-sessions = First work sessions…
activity-brief-underlying = { $count } underlying { $count ->
        [one] action
       *[other] actions
    }
activity-brief-open-explorer = Open this work session in Explorer →
activity-brief-show-affected = Show affected resources and actions ⌄
activity-browse-by = Browse by
activity-more-lenses = More lenses…
activity-first-lenses = First lenses…
activity-lens-everything = Everything
activity-lens-agent-tasks = Agent tasks  { $count }
activity-lens-unclassified = Unclassified  { $count }
activity-provenance = Provenance
activity-select-prompt = Select a work session or a range of actions
activity-select-hint = See who was involved, affected resources, and an exact before-and-after comparison.
activity-detail-open = Open
activity-detail-open-app = Open app
activity-detail-compare = Compare
activity-detail-undo = Undo
activity-detail-undo-task = Undo whole task
activity-detail-split = Split
activity-detail-merge = Merge
activity-detail-action-count = { $count } { $count ->
        [one] action
       *[other] actions
    }
activity-no-resource = No resource declared
activity-what-changed = What changed
activity-comparison-counts = { $actions } { $actions ->
        [one] action
       *[other] actions
    } · { $changes } { $changes ->
        [one] document change
       *[other] document changes
    }
activity-no-differences = No content differences in this selection
activity-comparison-more = +{ $count } more
activity-preview-unavailable = Preview unavailable
activity-preview-unavailable-detail = This action has provenance, but no materialized before-and-after snapshot.
activity-change-resolved-field = Resolved concurrent changes to { $field }
activity-change-updated-field = Updated { $field }
activity-change-removed-field = Removed { $field }
activity-change-resolved-list = Resolved concurrent list changes
activity-change-updated-list-item = Updated a list item
activity-change-added-items = Added { $count } { $count ->
        [one] item
       *[other] items
    }
activity-change-removed-items = Removed { $count } { $count ->
        [one] item
       *[other] items
    }
activity-change-edited-text = Edited text
activity-change-a-counter = a counter
activity-change-decreased = Decreased { $field }
activity-change-increased = Increased { $field }
activity-change-concurrent-field = Concurrent changes to { $field }
activity-change-concurrent-list = Concurrent list changes
activity-change-formatting = Changed text formatting
activity-change-content = content
activity-agent-task = Agent task
activity-agent-task-people = Assigned by { $issuer }  ·  Run by { $agent }
activity-agent-fallback = Agent
activity-unknown-collaborator = Unknown collaborator
activity-why-this-work = Why this work
activity-no-decision = No decision route was recorded.
activity-affected-records = Affected records
activity-undo-task-needs-open = Open every affected document to undo this task together.
activity-undo-task-needs-documents = Nothing changed. Open every affected document first.
activity-undo-task-partial = Task partly undone; later collaborator edits were kept.
activity-undo-task-done = Whole task undone as a new history entry.
activity-undo-task-gone = The task's effects were already gone.
activity-removed-workspace = a removed workspace
activity-receipt-completed = Completed
activity-receipt-failed = Failed
activity-receipt-cancelled = Cancelled
activity-replay-pending = Replay is waiting for current edits to finish
activity-replay-unavailable = Workspace replay is unavailable for these checkpoints
activity-replay-empty = No replayable workspace checkpoints yet
activity-replay-pause = Pause
activity-replay-play = Play
activity-replay-position = Checkpoint { $position } of { $total } · { $when }
activity-replay-no-workspace = The workspace did not exist at this checkpoint
activity-replay-empty-window = Empty window
activity-replay-layout-not-recorded = Window sizes and selected tabs were not recorded
activity-presence-trail = Presence trail
activity-tasks-subtitle = { $workspace } · { $active } active { $active ->
        [one] task
       *[other] tasks
    } · { $agents } assigned { $agents ->
        [one] agent
       *[other] agents
    }
activity-tasks-tagline = Native workspace tasks · model optional · every action auditable
activity-tasks-pending = Tasks are waiting for current edits to finish
activity-tasks-empty = No tasks yet
activity-tasks-empty-hint = Create one manually, or attach the resource selected in Explore.
activity-task-new = + New task
activity-task-queue = WORKSPACE QUEUE
activity-task-composer-title = Create work without leaving the desktop
activity-task-placeholder = What needs doing?
activity-task-executor-manual = Executor · You or another collaborator · no model
activity-task-executor-none = Executor · No agent available · task stays unassigned
activity-task-executor-agent = Executor · ▣ AGENT · { $agent }
activity-task-no-attachment = No resource attached · task stays workspace-wide
activity-task-attached = Attached · { $app } · { $resource }
activity-task-attached-workspace = Workspace
activity-task-cancel-compose = Cancel
activity-task-create = Create
activity-task-open-resource = Open resource
activity-task-start-agent = Start agent
activity-task-launch-agent = Launch agent
activity-task-mark-started = Mark started
activity-task-mark-done = Mark done
activity-task-cancel = Cancel task
activity-task-retry-agent = Retry agent
activity-task-audit-trail = AUDIT TRAIL
activity-task-unassigned-by = Unassigned · created by { $creator }
activity-task-agent-badge = ▣ AGENT · { $name }
activity-task-status-queued = Queued
activity-task-status-in-progress = In progress
activity-task-status-waiting = Waiting
activity-task-status-done = Done
activity-task-status-cancelled = Cancelled
activity-task-route-manual = Manual · no model
activity-task-route-rules-only = Agent · rules only
activity-task-route-model = Agent · model assisted
activity-task-notice-no-agent = No enabled workspace agent is assigned · this task has not started
activity-task-notice-ready = Agent connected · ready to run this task
activity-task-notice-starting = Starting managed agent · waiting for its broker subscription
activity-task-notice-offline = Agent offline · { $detail }
activity-task-notice-no-runtime = Agent runtime not connected · this task has not started
activity-agent-connect-timeout = The agent did not connect to the workspace. Retry to start a new managed session.
activity-agent-stopped = The managed agent stopped before connecting.
activity-agent-exited = The managed agent launcher exited with status { $code }.
activity-agent-dispatch-failed = agent task dispatch failed
activity-agent-dispatch-unavailable = agent task dispatch is unavailable
activity-agent-launch-unavailable = agent launching is unavailable

# Workspace history events
activity-event-removed-workspaces = Removed { $count } { $count ->
        [one] workspace
       *[other] workspaces
    }
activity-event-recorded-workspaces = Recorded { $count } { $count ->
        [one] workspace
       *[other] workspaces
    }
activity-event-recorded-workspace = Recorded workspace “{ $name }”
activity-event-recorded-named-workspaces = Recorded { $count } workspaces: { $names }
activity-event-granted = Granted { $capability }
activity-event-granted-capabilities = Granted { $count } { $count ->
        [one] capability
       *[other] capabilities
    }
activity-event-revoked-capabilities = { $count ->
        [one] Revoked a capability
       *[other] Revoked { $count } capabilities
    }
activity-event-owner = Set the workspace owner
activity-event-identity = Assigned the workspace identity
activity-event-renamed = Renamed the workspace
activity-event-named = Named the workspace “{ $name }”
activity-event-opened-windows = Opened { $count } { $count ->
        [one] window
       *[other] windows
    }
activity-event-opened = Opened { $titles }
activity-event-closed-windows = Closed { $count } { $count ->
        [one] window
       *[other] windows
    }
activity-event-rearranged = Rearranged the strip
activity-event-resized = Resized a window group
activity-event-switched = Switched workspace
activity-titles-two = { $first } and { $second }
activity-titles-more = { $first }, { $second } and { $count } more
activity-capability = { $effect ->
        [restrict] restricted { $verb }
       *[grant] { $verb }
    } on { $scope } to { $subject }
activity-capability-unknown = a capability
activity-capability-this-workspace = this workspace
activity-capability-every-type = every { $kind }
activity-capability-one-artifact = one artifact
activity-capability-one-shared-file = one shared file
activity-capability-network = network { $host }
activity-capability-role = { $role ->
        [owner] owners
        [editor] editors
        [commenter] commenters
        [viewer] viewers
       *[denied] blocked members
    }
activity-capability-a-person = a person
activity-capability-agent = agent { $agent }
activity-capability-invite-token = an invite token

# Desktop surfaces
desktop-system-status = System status
desktop-app-unavailable = Application unavailable
desktop-app-identity-unavailable = Application identity unavailable
desktop-app-integration-unavailable = { $app }: application integration or required service unavailable
desktop-app-no-adapter = No semantic application adapter; process-local pixels cannot be reconstructed
desktop-view-switch-workspace = Switch to { $workspace }
desktop-view-review-activity = Review workspace activity
desktop-view-explore-activity = Explore workspace activity
desktop-view-replay-activity = Replay workspace activity


## Desktop window search
search-provider-desktop-windows = Desktop windows
search-badge-tab = Tab
search-badge-window-group = Window group
search-window-move-tab = Move tab “{ $tab }” to workspace “{ $workspace }”
search-window-move-tab-detail = App { $app } · from workspace “{ $workspace }” · group { $group } · tab { $tab }
search-window-group-untitled = Window group { $group }
search-window-close-group = Close window group “{ $group }”
search-window-close-group-detail = Workspace “{ $workspace }” · strip “{ $strip }” · group { $group } · { $count ->
    [one] 1 tab
   *[other] { $count } tabs
}
search-window-close-tab = Close tab “{ $tab }”
search-window-close-tab-detail = App { $app } · workspace “{ $workspace }” · strip “{ $strip }” · group { $group } · tab { $tab }
