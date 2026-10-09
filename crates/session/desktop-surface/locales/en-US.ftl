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

activity-filter-time = Time
activity-filter-people = People
activity-filter-apps = Apps
activity-filter-kind = Kind
activity-filter-all = All
activity-detail-restore = Restore
activity-history-compare = Compare this change
activity-history-undo = Undo this change
activity-history-restore = Restore this version
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
activity-session-move-newer = ↑  1 action
activity-session-move-older = 1 action  ↓
activity-more-sessions = More work sessions…
activity-first-sessions = First work sessions…
activity-brief-underlying = { $count } underlying { $count ->
        [one] action
       *[other] actions
    }
activity-brief-open-explorer = Open this work session in Explorer →
activity-brief-show-affected = Show affected resources and actions
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
activity-receipt-ended = { $status }: { $what }
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
activity-task-executor-agent = Executor · Agent · { $agent }
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
activity-task-audit-noted = { $what }: { $note }
workspace-create-task = { $manual ->
    [true] Created the task “{ $title }” to do by hand
   *[false] Created the task “{ $title }”
}
workspace-attach-resource = Attached { $resource } to the task
workspace-set-execution-budget = Set how long the task may run
workspace-start-run = Started run { $run }
workspace-add-direction = Gave direction { $revision }
workspace-apply-direction = Followed direction { $revision }
workspace-finish = { $outcome ->
    [done] Finished the task
    [waiting] Stopped to wait
   *[cancelled] Cancelled the task
}
workspace-abandon = Gave up the run
workspace-retry = Queued the task again
workspace-cancel = Cancelled the task
workspace-report = Reported progress
workspace-set-status = { $status ->
    [queued] Queued the task
    [in-progress] Started the task
    [waiting] Set the task to waiting
    [done] Marked the task done
   *[cancelled] Cancelled the task
}
activity-task-unassigned-by = Unassigned · created by { $creator }
activity-task-agent-badge = Agent · { $name }
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
activity-event-created = Created the workspace
activity-event-created-named = Created the workspace “{ $name }”
activity-event-updated = Updated the workspace
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
activity-capability-one-shared-file = one shared file
activity-capability-network = network { $host }
activity-capability-role = { $role ->
        [owner] owners
        [editor] editors
        [commenter] commenters
       *[viewer] viewers
    }
activity-grant = { $answer ->
        [deny] denied
        [ask] ask first for
       *[allow] allowed
    } { $kinds ->
        [read] reading
        [undoable] changes
        [outside] outside effects
        [read-undoable] reading and changes
        [read-outside] reading and outside effects
        [undoable-outside] changes and outside effects
        [none] nothing
       *[all] everything
    } on { $node } to { $subject }
activity-grant-task = task { $task }
activity-grant-shortcut = Shortcut { $shortcut }
activity-grant-foreign-device = devices that are not their own
activity-member = { $person } as { $role ->
        [owner] owner
        [editor] editor
        [commenter] commenter
       *[viewer] viewer
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
search-window-move-tab-detail = From workspace “{ $workspace }”
search-window-copy-tab = Copy tab “{ $tab }” to workspace “{ $workspace }”
search-window-group-untitled = Untitled window
search-window-close-group = Close window group “{ $group }”
search-window-close-group-detail = Workspace “{ $workspace }” · strip “{ $strip }” · { $count ->
    [one] 1 tab
   *[other] { $count } tabs
}
search-window-close-tab = Close tab “{ $tab }”
search-window-close-tab-detail = Workspace “{ $workspace }” · strip “{ $strip }”
search-window-focus-place = Open in this workspace
search-window-focus-place-strip = Open in strip “{ $strip }”
search-window-focus-detail-shared = { $place } · { $count ->
    [one] 1 tab
   *[other] { $count } tabs
}
# Words that ask for a window's close or move/copy rows; a search for a
# window by name only offers going to it. Space-separated, lowercase.
search-window-verb-close = close quit
search-window-verb-relocate = move copy send

# Notifications: banner stack and notification centre
drill-in-back = Back
a11y-notifications-none = No retained notifications.
a11y-notifications-description = Showing newest rows { $first } through { $last } of { $total }, grouped by app. Use Up and Down to select a notification, Page Up and Page Down to move a screen, and Home or End to jump to the newest or oldest retained notification. Enter opens the selected notification or folds its app group, and Delete dismisses it.
a11y-notifications-list = Retained notifications
a11y-notifications-clear = Clear all notifications
a11y-notifications-clear-description = Delete this session's retained notification history. An Undo action remains available for eight seconds.
a11y-notifications-cleared = Notifications cleared
a11y-notifications-undo = Undo clear notifications
a11y-notifications-undo-description = Restore the cleared notifications in their original order. Press Enter or Space.
a11y-notifications = Notifications
notification-action-open = Open
notification-center-title = Notifications
notification-center-empty = No recent notifications
notification-center-cleared = Notifications cleared
notification-center-clear-all = Clear all
notification-center-position = Newest { $first }–{ $last } of { $total }
notification-center-count = { $count ->
        [one] { $count } notification
       *[other] { $count } notifications
    }
notification-center-newer = ↑ Newer
notification-center-at-newest = At newest
notification-center-older = ↓ Older
notification-center-at-oldest = At oldest
history-severity-info = Information
history-severity-warning = Warning
history-severity-error = Error
history-source-system = System
history-notification-description = { $severity }. From { $source }. { $time }
history-notification-description-body = { $severity }. From { $source }. { $time }. { $body }
notification-center-group-description = { $count } notifications. Press Enter to { $expanded ->
        [yes] collapse
       *[no] expand
    }, Delete to dismiss them all.
notification-center-dismiss-all = Dismiss all from { $app }
notification-center-dismiss = Dismiss
notification-center-hand-to = Hand to { $agent }
notification-center-hand-to-description = Give this notification to the workspace agent as a task.
notification-center-group-count = { $count ->
        [one] { $count } notification
       *[other] { $count } notifications
    }
notification-lock-count = { $count ->
    [one] 1 notification
   *[other] { $count } notifications
}
notification-dnd-held = { $status } · { $count } held
notification-stack-a11y = Notifications
notification-group-system = System
notification-center-undo = Undo
notification-app-failed-title = Couldn't open an app
notification-service-unavailable = This isn't available on this desktop yet

# Workspace settings
workspace-settings-roles-resource = { $workspace } permission roles
workspace-settings-role-save = Create or update permission role
workspace-settings-role-save-placeholder = Role name
workspace-settings-role-save-description = Save a reusable least-privilege role made from app, action, file, resource, or workspace capability rules.
workspace-settings-role-assign = Assign permission role
workspace-settings-role-assign-placeholder = Role and collaborator
workspace-settings-role-assign-description = Assign a saved least-privilege role to a person, application, or agent.
workspace-settings-role-assign-named = Assign { $role }
workspace-settings-role-assign-named-placeholder = Collaborator identity
workspace-settings-app-action-placeholder = App, resource and action
workspace-settings-app-action-grant = Let an app use another app's action
workspace-settings-app-action-grant-description = Grant one application the use of one action on one resource another application publishes, in this workspace.
workspace-settings-app-action-revoke = Stop an app using another app's action
workspace-settings-app-action-revoke-description = Take back every grant letting one application use one action on one of another application's resources in this workspace.
workspace-settings-ownership-resource = { $workspace } ownership
workspace-settings-ownership-propose = Propose ownership transfer
workspace-settings-ownership-propose-placeholder = Recipient identity
workspace-settings-ownership-propose-description = Your signature authorizes ownership to transfer immediately if this exact administrator accepts before expiry.
workspace-settings-ownership-accept = Accept workspace ownership
workspace-settings-ownership-accept-description = Acceptance immediately transfers ownership; generic role controls cannot perform this action.
workspace-settings-ownership-accept-phrase = Accept ownership
workspace-settings-ownership-cancel = Cancel ownership transfer
workspace-settings-ownership-cancel-description = Cancel the pending offer before the recipient accepts it.
workspace-settings-title = Workspace Settings
workspace-settings-close = Close Settings
workspace-settings-tab-general = General
workspace-settings-tab-people = People
workspace-settings-tab-agents = Agents
workspace-settings-tab-privacy-security = Privacy & Security
workspace-settings-tab-privacy = Privacy
workspace-settings-tab-apps-files = Apps & Files
workspace-settings-tab-apps = Apps
workspace-settings-close-hint = Press Esc to close
workspace-settings-edit-hint = Enter to save · Esc to cancel
workspace-settings-role-owner = Owner
workspace-settings-role-editor = Editor
workspace-settings-role-commenter = Commenter
workspace-settings-role-viewer = Viewer
workspace-settings-role-blocked = Blocked
workspace-settings-role-admin = Admin
workspace-settings-role-guest = Guest
workspace-settings-role-owner-preview = Can manage people, permissions, and workspace settings.
workspace-settings-role-editor-preview = Can view, comment on, and edit shared work.
workspace-settings-role-commenter-preview = Can view shared work and add comments, but cannot edit it.
workspace-settings-role-viewer-preview = Can view shared work, but cannot comment or edit it.
workspace-settings-role-blocked-preview = Cannot access this workspace.
workspace-settings-scope-workspace = this workspace
workspace-settings-scope-app = the { $app } app
workspace-settings-scope-workspace-files = workspace files
workspace-settings-scope-workspace-folder = “{ $folder }” in this workspace
workspace-settings-scope-path = files under { $path }
workspace-settings-scope-record = one shared file ({ $id })
workspace-settings-scope-camera = the camera
workspace-settings-scope-microphone = the microphone
workspace-settings-scope-screencast = screen capture
workspace-settings-scope-remote-desktop = remote pointer + keyboard
workspace-settings-scope-any-host = any network host
workspace-settings-scope-host = network host { $host }
workspace-settings-scope-action = “{ $action }” on { $resource } in { $app }
workspace-settings-scope-action-on-kind = “{ $action }” on every { $kind } in { $app }
workspace-settings-scope-resource-type = { $kind ->
    [note] every note
    [file] every file
    [cluster] every saved cluster
    [kanban] every board
   *[chat-log] every chat log
}
workspace-settings-scope-resource-instance = one document ({ $id })
workspace-settings-scope-workspace-object = this workspace
workspace-settings-scope-app-object = the { $app } app
workspace-settings-scope-workspace-files-object = workspace files
workspace-settings-scope-workspace-folder-object = “{ $folder }” in this workspace
workspace-settings-scope-path-object = files under { $path }
workspace-settings-scope-record-object = one shared file ({ $id })
workspace-settings-scope-camera-object = the camera
workspace-settings-scope-microphone-object = the microphone
workspace-settings-scope-screencast-object = screen capture
workspace-settings-scope-remote-desktop-object = remote pointer + keyboard
workspace-settings-scope-any-host-object = any network host
workspace-settings-scope-host-object = network host { $host }
workspace-settings-scope-action-object = “{ $action }” on { $resource } in { $app }
workspace-settings-scope-action-on-kind-object = “{ $action }” on every { $kind } in { $app }
workspace-settings-scope-resource-type-object = { $kind ->
    [note] every note
    [file] every file
    [cluster] every saved cluster
    [kanban] every board
   *[chat-log] every chat log
}
workspace-settings-scope-resource-instance-object = one document ({ $id })
workspace-settings-allows-control = Control { $what }
workspace-settings-allows-use = Use { $what }
workspace-settings-allows-reach = Reach { $what }
workspace-settings-allows-run = Run { $what }
workspace-settings-allows-administer = Administer { $what }
workspace-settings-allows-edit = Edit { $what }
workspace-settings-allows-comment = Comment on { $what }
workspace-settings-allows-invite = Invite others to { $what }
workspace-settings-allows-view = View { $what }
workspace-settings-grant-allows = Allowed: { $kinds } in { $where }
workspace-settings-grant-asks = Asks first: { $kinds } in { $where }
workspace-settings-grant-denies = Refused: { $kinds } in { $where }
workspace-settings-kind-read = reading
workspace-settings-kind-undoable = changes
workspace-settings-kind-outside = outside effects
workspace-settings-subject-foreign-device = devices that are not their own
workspace-settings-grant-placeholder = Who, where and what
workspace-settings-grant-description = Give someone, an agent or an app an answer for kinds of action in part of this workspace.
workspace-settings-revoke-placeholder = Which grant
workspace-settings-revoke-description = Take back one grant by its id.
workspace-settings-expiry-once = Once
workspace-settings-expiry-session = Session
workspace-settings-expiry-until = Until
workspace-settings-expiry-always = Always
workspace-settings-granted-by-person = granted by { $id }
workspace-settings-granted-by-you = granted by you
workspace-settings-granted-by-app = granted by the { $app } app
workspace-settings-granted-by-agent = granted by agent { $id }
workspace-settings-granted-by-token = granted by token { $id }
workspace-settings-granted-by-workspace = granted by workspace { $id }
workspace-settings-permission-detail = { $allows } · { $grantor }
workspace-settings-section-apps = Apps
workspace-settings-section-app-actions = App actions
workspace-settings-section-people = People
workspace-settings-section-agents = Agents
workspace-settings-section-roles = Roles
workspace-settings-section-other = Other
workspace-settings-subject-role = everyone with { $role }
workspace-settings-subject-token = token { $id }
workspace-settings-subject-workspace = workspace { $id }
workspace-settings-grant-camera = Camera
workspace-settings-grant-microphone = Microphone
workspace-settings-grant-screencast = Screen capture
workspace-settings-grant-remote-control = Remote control
workspace-settings-grant-network = Network access
workspace-settings-grant-read-files = Read workspace files
workspace-settings-grant-edit-files = Edit workspace files
workspace-settings-grant-create-roles = Create permission roles
workspace-settings-grant-assign-roles = Assign permission roles
workspace-settings-preset-observer = Observer
workspace-settings-preset-contributor = Contributor
workspace-settings-preset-operator = Operator
workspace-settings-preset-custom = Custom
workspace-settings-preset-observer-hint = Read only on surfaces you select
workspace-settings-preset-contributor-hint = Read/write only on surfaces you select
workspace-settings-preset-operator-hint = Run only approved app actions
workspace-settings-preset-custom-hint = Review every scoped permission
workspace-settings-members-count = { $count ->
    [one] 1 person shares this workspace
   *[other] { $count } people share this workspace
}
workspace-settings-members-help = Removing someone blocks future sync and edits; copies already downloaded stay with them.
workspace-settings-ownership = Ownership
workspace-settings-you-manage = You manage
workspace-settings-view-only = View only
workspace-settings-members-empty-title = No one else is in this workspace
workspace-settings-members-empty-detail = Create a signed invite from the General tab to add someone.
workspace-settings-people-header = PEOPLE
workspace-settings-ownership-offered = { $owner } offered you ownership. Review the workspace and explicitly accept before the offer expires.
workspace-settings-ownership-waiting = Waiting for { $recipient } to explicitly accept ownership. You can cancel until acceptance.
workspace-settings-ownership-pending = Ownership transfer to { $recipient } is awaiting that person's acceptance.
workspace-settings-you = You
workspace-settings-member-detail = { $count ->
    [0] { $role } · no extra permissions
    [one] { $role } · 1 extra permission
   *[other] { $role } · { $count } extra permissions
}
workspace-settings-remove = Remove
workspace-settings-owner-only = Only the workspace owner can change permissions.
workspace-settings-agents-count = { $count ->
    [one] 1 agent collaborator
   *[other] { $count } agent collaborators
}
workspace-settings-agents-help = Agents are visibly non-human collaborators. Admission grants no ambient workspace power.
workspace-settings-add-agent = Add agent
workspace-settings-agents-empty-title = No agents have access
workspace-settings-agents-empty-detail = Start with a safe profile, then review each exact surface.
workspace-settings-agents-header = AGENTS
workspace-settings-service-member = Service member · { $access ->
    [admin] administers
    [write] reads and edits
    [comment] reads and comments
   *[read] reads
} end-to-end encrypted content · { $until } · device { $device }
workspace-settings-service-until-removed = until removed
workspace-settings-service-ends-soon = access ends within a minute
workspace-settings-service-ends-minutes = access ends in { $minutes } min
workspace-settings-service-ends-hours = access ends in { $hours } h
workspace-settings-badge-service = SERVICE
workspace-settings-badge-agent = AGENT
workspace-settings-agent-detail = { $count ->
    [0] { $admission } · no scoped permissions — review required
    [one] { $admission } · { $profile } profile · 1 scoped permission
   *[other] { $admission } · { $profile } profile · { $count } scoped permissions
}
workspace-settings-admitted = Admitted
workspace-settings-review-access = Review access
workspace-settings-revoke = Revoke
workspace-settings-admission-admin = Admin admission
workspace-settings-admission-write = Write admission
workspace-settings-admission-comment = Comment admission
workspace-settings-admission-read = Read admission
workspace-settings-unavailable-app = Unavailable app
workspace-settings-apps-empty-title = No file handlers installed
workspace-settings-apps-empty-detail = Apps appear here after they declare supported file types.
workspace-settings-apps-intro-title = Defaults without lock-in
workspace-settings-apps-intro-detail = The workspace supplies a baseline; each person can explicitly choose something else.
workspace-settings-apps-header = DEFAULT APPLICATIONS
workspace-settings-workspace-choice = Workspace choice
workspace-settings-your-choice = Your choice
workspace-settings-resource-extension = .{ $ext } files
workspace-settings-resource-scheme = { $scheme }: links
workspace-settings-follow-personal = Follow personal defaults
workspace-settings-use-workspace-choice = Use workspace choice
workspace-settings-use-personal-default = Use my personal default
workspace-settings-apps-file-types = { $count ->
    [one] 1 file type
   *[other] { $count } file types
}
workspace-settings-apps-custom-rules = { $count ->
    [one] 1 custom rule
   *[other] { $count } custom rules
}
workspace-settings-permissions-intro-title = Access is explicit and revocable
workspace-settings-permissions-intro-detail = Grants are enforced by the app sandbox and stay scoped to this workspace.
workspace-settings-active-grants = { $count ->
    [one] 1 active grant
   *[other] { $count } active grants
}
workspace-settings-permissions-empty-title = No permissions granted
workspace-settings-permissions-empty-detail = Apps get nothing until it is granted here or approved in a prompt.
workspace-settings-more = +{ $count } more
workspace-settings-who = Who
workspace-settings-who-agent = Who · AGENT (only this surface; no ambient workspace access)
workspace-settings-nobody-to-grant = Nobody to grant to yet — invite someone, or open an app here.
workspace-settings-what-it-allows = What it allows
workspace-settings-grant-header = GRANT A PERMISSION
workspace-settings-saved-roles = Manage roles ({ $count })
workspace-settings-expiry-caption = Until
workspace-settings-grant = Grant
workspace-settings-role-sheet-title = Choose workspace access
workspace-settings-role-sheet-description = { $person } is currently { $role }. Choose exactly what they can do.
workspace-settings-remove-sheet-title = Remove this person?
workspace-settings-remove-sheet-description = They will stop receiving future sync and edits. Downloaded copies remain on their devices.
workspace-settings-revoke-sheet-title = Revoke this permission?
workspace-settings-revoke-sheet-description = The selected capability will stop applying immediately. Review its activity first if you are unsure.
workspace-settings-keep-access = Keep access
workspace-settings-review-activity = Review activity first
workspace-settings-remove-person = Remove person
workspace-settings-no-changes = No changes are made.
workspace-settings-open-history = Open workspace history before deciding.
workspace-settings-remove-person-detail = This removes their workspace capabilities.
workspace-settings-keep-permission = Keep permission
workspace-settings-revoke-permission = Revoke permission
workspace-settings-revoke-permission-detail = This removes the selected capability immediately.
workspace-settings-workspace-fallback = Workspace
workspace-settings-identity-caption = Workspace · { $role }
workspace-settings-current-workspace = CURRENT WORKSPACE
workspace-settings-role-policy-line = { $role } · Settings and policy stay with this workspace
workspace-settings-identity-title = Workspace identity
workspace-settings-identity-description = This name appears in the top bar, invites, and shared activity.
workspace-settings-rename-admin-only = Only a workspace administrator can rename this workspace.
workspace-settings-invite-hint = Choose a person and role; Tilo signs a one-hour invite.
workspace-settings-invite-admin-only = Only a governing administrator can create workspace invites.
workspace-settings-invite-create = Create
workspace-settings-devices-title = Your devices
workspace-settings-device-here = { $device } (this device)
workspace-settings-device-shown = Shown
workspace-settings-device-hidden = Kept off
workspace-settings-move-earlier = Move up
workspace-settings-move-later = Move down
workspace-settings-delete = Delete workspace
workspace-settings-invite-title = Invite people
workspace-settings-invite-description = Invites bind the workspace, recipient role, expiry and this device's identity.
workspace-settings-glance-title = At a glance
workspace-settings-stat-members = { $count ->
    [one] member
   *[other] members
}
workspace-settings-stat-grants = { $count ->
    [one] specific grant
   *[other] specific grants
}
workspace-settings-stat-app-rules = { $count ->
    [one] app rule
   *[other] app rules
}

# People popover
a11y-shell-people = People
a11y-shell-people-description = Live workspace presence, current activity, intent, and status privacy controls
a11y-shell-people-screen-visible = Screen visible now: { $exposure }
a11y-shell-people-here-now = People here now
a11y-shell-people-agent = Agent
a11y-shell-people-invited = Invited, not currently present
a11y-shell-people-present = Present now
a11y-shell-people-now = Now: { $activity }
a11y-shell-people-intent = Intent: { $status }
a11y-shell-people-intent-inferred = Intent, inferred: { $status }
a11y-shell-status-name = Your collaboration status
a11y-shell-status-inferred = Inferred on this device from recent semantic actions
a11y-shell-status-set = Set what you are trying to accomplish
a11y-shell-status-automatic = Automatic status
a11y-shell-status-manual = Manual status only
a11y-shell-status-off = Hide status
a11y-shell-status-automatic-description = Infer a short status locally from recent workspace actions
a11y-shell-status-manual-description = Share only text you enter yourself
a11y-shell-status-off-description = Publish no status to this workspace
a11y-shell-follow-collaborator = Follow collaborator
a11y-shell-follow = Follow { $name }
a11y-shell-follow-description = Mirror this participant's live workspace view
a11y-shell-people-previous = Previous people
a11y-shell-people-previous-description = Show the previous people in this workspace
a11y-shell-people-next = Next people
a11y-shell-people-next-description = Show the next people in this workspace
a11y-shell-invite = Invite people
a11y-shell-invite-description = Create an authenticated workspace invitation
a11y-shell-join = Join workspace
a11y-shell-join-description = Connect using an authenticated invitation
a11y-shell-workspace-activity-description = Review grouped outcomes and what happened while you were away

# Empty-workspace launchpad
launchpad-title = Make something together
launchpad-description = Choose a shared conversation, notes, tasks, a call, an invitation, or workspace activity.
launchpad-open-app = Open an app
launchpad-discuss = Discuss
launchpad-notes = Notes
launchpad-tasks = Tasks
launchpad-meet = Start a call
launchpad-invite = Invite people
launchpad-activity = Catch up
launchpad-discuss-detail = Open the shared conversation
launchpad-notes-detail = Write ideas together
launchpad-tasks-detail = Open the to-do list in Calendar
launchpad-meet-detail = Talk with this workspace in Chat
launchpad-invite-detail = Bring someone into this workspace
launchpad-activity-detail = See what changed and why

# Empty-workspace launchpad: the keyboard hint under the cards
launchpad-keyboard-hint = Arrow keys move  ·  Enter opens  ·  Super+N shows every app

# Peers arriving in and leaving a workspace, batched into one toast
peer-toast-joined-one = { $name } joined
peer-toast-left-one = { $name } left
peer-toast-joined-many = { $count } peers joined
peer-toast-left-many = { $count } peers left
peer-toast-overflow = { $first }, { $second }, { $third }, +{ $rest } more

# Moved from the shell catalogue with the code that uses them
people-panel-here-now = { $count } here now
people-panel-screen-visible = Screen visible now
people-panel-only-you = Only you are here
people-panel-status-typing = Type a status, then press Enter
people-panel-status-empty = Set what you're working toward…
people-panel-own-status = You · { $status }
people-panel-sharing-auto = Auto
people-panel-previous-page = Previous people
people-panel-next-page = Next people
people-panel-sharing-manual = Manual
people-panel-sharing-hidden = Hidden
people-panel-agent-tag = agent
people-panel-invited-tag = invited
people-panel-following = Following
people-panel-following-activity = Following · { $activity }
people-panel-viewing = Viewing { $title }
people-panel-follow = Follow
people-panel-stop-following = Stop
people-panel-invite = Invite
people-panel-join = Join
launchpad-subtitle = Choose how to begin. Workspace content syncs; your layout and controls stay local.
workspace-settings-sheet-keyboard-hint = ↑ ↓ to choose · Enter to confirm · Esc to cancel

# The emoji picker (Super+. by default)
emoji-picker-title = Emoji
emoji-picker-search = Search emoji
emoji-picker-groups = Emoji categories
emoji-picker-recent = Recently used
emoji-picker-results = Matching emoji
emoji-picker-no-results = No emoji found
emoji-picker-hint = Enter inserts · Esc closes
emoji-picker-hint-tones = Enter inserts · Shift+Enter picks a skin tone
emoji-picker-tones = Skin tones
emoji-picker-tones-hint = Right-click, long-press or Shift+Enter for skin tones
emoji-picker-a11y-description = Type to search. The arrow keys move between emoji, Tab changes the category, Enter inserts the highlighted emoji into the text field you were typing in, Shift+Enter picks a skin tone, Escape closes.

# An emoji in a skin tone: "thumbs up, medium skin tone".
emoji-picker-toned = { $name }, { $tone }
emoji-picker-tone-light = light skin tone
emoji-picker-tone-medium-light = medium-light skin tone
emoji-picker-tone-medium = medium skin tone
emoji-picker-tone-medium-dark = medium-dark skin tone
emoji-picker-tone-dark = dark skin tone

# The Alt+Tab window switcher.
window-switcher-title = Switch windows
window-switcher-description = Windows in this workspace, most recently used first. Hold Alt and press Tab or the arrow keys to move, release Alt to switch, or press Escape to stay.
window-switcher-app-title = Switch windows of this app
window-switcher-app-description = This app's windows in this workspace, most recently used first. Hold Alt and press ` to move, release Alt to switch, or press Escape to stay.
window-switcher-untitled = Untitled window
window-switcher-caption-paged = { $title } ({ $position } of { $count })

# Workspace wheel
a11y-shell-wheel = Workspace wheel
a11y-shell-wheel-switch = Switch to { $workspace }
a11y-shell-wheel-add = Add a workspace

# Workspace overview
overview-empty-window = Empty window
overview-untitled = Untitled
overview-access-nobody = No one has access
overview-access-private = Private to you
overview-access-one = Shared with { $name }
overview-access-two = Shared with { $name } and { $other }
overview-access-many = Shared with { $name } and { $count } others
overview-shares = { $count ->
        [one] 1 item shared
       *[other] { $count } items shared
    }
overview-presence-you = You are here
overview-presence-you-and-one = You and { $name } are here
overview-presence-you-and-many = You and { $count } others are here
overview-presence-one = { $name } is here
overview-presence-many = { $count } people are here
overview-collaborators-here = { $count } collaborators are here

# The launchpad in the browser: a new member's first look at the desktop
browser-welcome-title = Welcome to Tilo
browser-welcome-subtitle = Your team's shared computer. Everything you make is saved as you go and end-to-end encrypted.
browser-welcome-hint = Arrow keys move  ·  Enter opens  ·  Ctrl+K finds any app
browser-welcome-hint-mac = Arrow keys move  ·  Enter opens  ·  ⌘K finds any app

# The overview panel a host without its own overview draws
overview-panel-title = Overview
overview-panel-you-are-here = You are here
overview-panel-windows = Windows here
overview-panel-no-windows = No windows yet
overview-panel-more-windows = { $count ->
    [one] and 1 more window
   *[other] and { $count } more windows
}
overview-panel-window-count = { $count ->
    [one] 1 window
   *[other] { $count } windows
}
overview-panel-go-to = Go to
overview-panel-new-workspace = New workspace
browser-new-workspace-name = Workspace { $number }

notification-center-read = Read
notification-center-read-description = Read complete notification
notification-preview-hidden = Notification preview hidden
activity-catch-up-goals = Workspace tasks · { $done } completed · { $unfinished } unfinished · updated { $when }
activity-catch-up-review-description = Review this workspace's waiting tasks, goals and agent work. Reading activity does not resolve a task.
activity-catch-up-review-goals = Review goals and agent work
activity-acknowledge-viewed = Acknowledge viewed activity
activity-acknowledge-description = Mark only the displayed summaries as read. Keep hidden changes and waiting tasks pending.
activity-catch-up-needs-review = Needs your review · { $count ->
    [one] one waiting task
   *[other] { $count } waiting tasks
    }
activity-catch-up-waiting-detail = { $task } · { $who }

notification-alarm-snoozed = Snoozed for 5 minutes. Dismiss this entry to cancel the pending snooze.
notification-alarm-dismiss = Dismiss
notification-alarm-snooze = Snooze for 5 minutes
notification-alarm-action-failed = Could not change this alarm
notification-alarm-action-title = { $action }: { $title }
notification-alarm-action-subtitle = For this alarm occurrence
notification-alarm-action-retry = Keep this notification and try again.

activity-task-review-unavailable = Authenticated task controls are unavailable. Existing work history remains readable.
activity-task-review-history = Review action history
activity-task-steer = Change instructions
activity-task-cancel-run = Cancel this run
activity-task-retry-run = Retry task
activity-task-direction-send = Send instructions
activity-task-direction-close = Back to task
activity-task-direction-label = Public task instructions
activity-task-direction-placeholder = Describe the change you want the agent to make
activity-task-direction-observation = Instructions for run { $epoch }, after revision { $revision }. A changed run requires another review.
activity-task-review-saving = Waiting for the workspace owner to save…
activity-task-review-saved = Saved by the workspace owner. Agent application is shown separately.
activity-task-review-refused = Not saved. Your draft is retained. { $detail }

activity-task-next = Next task
activity-task-previous = Previous task
activity-task-review-source = Workspace owner · generation { $generation } · sampled { $time }
activity-task-review-status = { $status } · Agent: { $agent }
activity-task-review-frontier = Run { $epoch } · instructions { $revision } · agent applied { $applied }
activity-task-review-direction = { $author }: { $text }
activity-task-review-missing = This task is no longer available in this workspace.
