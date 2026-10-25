# Top bar
continuity-dismiss = Dismiss
topbar-workspace-fallback = Workspace
topbar-sharing-agent-controls = Agent in control: { $name }
topbar-sharing-agent-viewing = Agent watching: { $name }
topbar-sharing-person-controls = In control: { $name }
topbar-sharing-person-viewing = Watching: { $name }
topbar-sharing-session-controls = In control: { $name }
topbar-sharing-session-viewing = Watching: { $name }
topbar-sharing-count-controlling = { $viewers } watching, { $controllers } in control
topbar-sharing-count = { $viewers } watching
topbar-sharing-unknown-window = a window
topbar-sharing-window-in-workspace = { $window } in { $workspace }
topbar-watching-count = +{ $count } watching
topbar-watching-description = { $count } { $count ->
        [one] person
       *[other] people
    } watching without joining
topbar-tilo-menu = Tilo menu
topbar-open-quick-settings = Open quick settings
topbar-open-notifications = Notifications
topbar-bell = { $unread ->
    [0] Notifications
    [one] Notifications, 1 unread
   *[other] Notifications, { $unread } unread
}
topbar-bell-silenced = { $unread ->
    [0] Notifications silenced
    [one] Notifications silenced, 1 unread
   *[other] Notifications silenced, { $unread } unread
}
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
topbar-invite = Invite
topbar-waved-at-you = { $name } waved at you
topbar-arrived-one = { $name } is here
topbar-arrived-more = { $name } and { $count ->
    [one] one other
   *[other] { $count } others
} are here
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
topbar-strip-name = Show strips, now on { $strip }
topbar-workspace-name-placeholder = Name the workspace
topbar-workspace-menu = { $workspace }: workspaces, overview and strips
topbar-profile = { $name } (you): your profile
topbar-face-agent = { $name }, agent
topbar-faces-more = { $count } more
workspace-menu-hosted-by = Hosted by { $host }
workspace-menu-rename = Rename workspace
workspace-menu-overview = Overview
workspace-menu-new-strip = New strip
profile-card = Your profile
profile-name = Your name
profile-name-placeholder = Your name
profile-name-hint = How everyone here sees you
profile-watching-hint = You’re watching. Nobody here sees you
profile-add-your-name = Add your name
profile-section-identity = Your identity
profile-section-devices = Devices
profile-section-linked = Linked accounts
profile-link = Link { $service } account
profile-unlink = Unlink
profile-copy = Copy
profile-sharing = Share what you're doing
profile-sharing-auto = Automatically
profile-sharing-manual = My words
profile-sharing-off = Not at all
profile-section-status = Status
profile-available = Available
profile-focusing = Focusing
profile-away = Away
profile-status = What you're doing
profile-status-placeholder = What you're doing
profile-section-colour = Your colour
profile-section-visibility = What people in { $workspace } see
profile-visibility-where = Where I am
profile-visibility-here = That I'm here
profile-visibility-nothing = Nothing
profile-visibility-where-detail = Your face, the window you're in and your pointer
profile-visibility-here-detail = Your face, never which window or your pointer
profile-visibility-nothing-detail = You don't appear, and you don't see others' pointers

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
a11y-shell-activity-open-tasks-description = Opens the Tasks app, where this workspace's tasks are
a11y-shell-activity-time-range = Time range
a11y-shell-activity-person-filter = Person filter
a11y-shell-activity-brief-description = { $detail }; last changed { $when }; { $count } { $count ->
        [one] change
       *[other] changes
    } by { $authors }
a11y-shell-activity-history-lens = History lens
a11y-shell-activity-undo-task-description = Undo every affected record in this agent task; later collaborator edits are kept
a11y-shell-activity-history-action = Selected history action
a11y-shell-activity-explorer-panel = Explorer panel
a11y-shell-activity-timeline-description = History entries; use arrow keys to navigate
a11y-shell-activity-checkpoints-description = Move through workspace checkpoints
a11y-shell-activity-replay-description = Play or pause the workspace replay
a11y-shell-activity-view-catch-up = A summary of outcomes since this device last viewed the workspace
a11y-shell-activity-view-explore = The complete workspace history with filters and provenance
a11y-shell-activity-view-replay = A causal replay of workspace layout and participant activity
a11y-shell-activity-entry-author = By { $author }
a11y-shell-activity-selected-detail = Selected history detail
a11y-shell-activity-selected-detail-description = { $title }; { $count } { $count ->
        [one] action
       *[other] actions
    }; by { $authors }
a11y-shell-activity-agent-run-description = Instruction: { $instruction }; assigned by { $issuer }; run by { $agent }; reason: { $reason }
a11y-shell-activity-reason-not-recorded = not recorded
a11y-shell-activity-agent-step = { $app } in { $record }: { $summary }
a11y-shell-activity-agent-review = Agent task review
activity-view-catch-up = What's new
activity-view-tasks = Tasks
activity-view-explore = History
activity-view-replay = Replay
activity-title-catch-up = While you were away
activity-title-whats-new = What's new
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
activity-resume-heading = Where you left off
activity-resume-context = { $app } · { $when }
activity-resume-button = Resume
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
activity-history-gone = These changes are no longer in the history.
activity-served-personal-only = Only the person at this device can do this.
activity-served-read-only = This desktop shows the history but can't change it.
activity-served-unavailable = This workspace's history isn't kept here.
activity-summary-on-device = On-device summary
activity-session-move-newer = 1 action newer
activity-session-move-older = 1 action older
activity-more-sessions = More work sessions…
activity-first-sessions = First work sessions…
activity-brief-underlying = { $count } { $count ->
        [one] step
       *[other] steps
    }
activity-brief-open-explorer = Open this work session in History
activity-brief-show-affected = Show each step
activity-browse-by = Browse by
activity-more-lenses = More lenses…
activity-first-lenses = First lenses…
activity-lens-everything = Everything
activity-lens-agent-tasks = Agent tasks  { $count }
activity-lens-unclassified = Unclassified  { $count }
activity-provenance = Provenance
activity-select-prompt = Select a work session or a range of actions
activity-select-hint = See who did it, what it touched, and compare before and after.
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
activity-undo-task-refused = Couldn't undo the task. { $detail }
activity-undo-task-unavailable = Undoing a task is unavailable here.
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
workspace-create-task = { $manual ->
    [true] Created the task “{ $title }” to do by hand
   *[false] Created the task “{ $title }”
}
workspace-attach-resource = Attached { $resource } to the task
workspace-set-execution-budget = Set how long the task may run
workspace-approve-run = Approved run { $run }
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
        [moderator] moderators
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
activity-grant-bot = Bot { $bot }
activity-grant-foreign-device = devices that are not their own
activity-member = { $person } as { $role ->
        [owner] owner
        [moderator] moderator
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
search-window-move-tab = Move tab “{ $tab }” to workspace “{ $workspace }”
search-window-move-tab-detail = From workspace “{ $workspace }”
search-window-copy-tab = Copy tab “{ $tab }” to workspace “{ $workspace }”
search-window-move-tab-to = Move “{ $tab }” to another workspace
search-window-copy-tab-to = Copy “{ $tab }” to another workspace
search-window-group-untitled = Untitled window
search-window-nth = { $nth } of { $count }
search-window-placeholder = Search { $searched }…
search-window-close-group = Close window group “{ $group }”
search-window-close-group-detail = Workspace “{ $workspace }” · strip “{ $strip }” · { $count ->
    [one] 1 tab
   *[other] { $count } tabs
}
search-window-close-tab = Close tab “{ $tab }”
search-window-close-tab-detail = Workspace “{ $workspace }” · strip “{ $strip }”
search-window-focus-place-strip = Strip “{ $strip }”
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
a11y-notifications-newer = Newer notifications
a11y-notifications-older = Older notifications
a11y-notifications = Notifications
notification-center-title = Notifications
notification-center-empty = No recent notifications
notification-center-cleared = Notifications cleared
notification-center-clear-all = Clear all
notification-center-position = { $first }–{ $last } of { $total }
notification-center-close = Close notifications
notification-center-back-to-list = Back to notifications
notification-center-newer = Newer
notification-center-older = Older
history-severity-info = Information
history-severity-warning = Warning
history-severity-error = Error
history-notification-description = { $severity }. { $time }
history-notification-description-body = { $severity }. { $time }. { $body }
history-notification-description-sourced = { $severity }. From { $source }. { $time }
history-notification-description-sourced-body = { $severity }. From { $source }. { $time }. { $body }
host-action-source-identity = Identity
host-action-source-devices = Devices
host-action-source-workspace = Workspace
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
notification-more = { $count } more
notification-stack-a11y = Notifications
notification-group-system = System
notification-center-undo = Undo
notification-not-now = Not now
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
workspace-settings-role-owner = Owner
workspace-settings-role-moderator = Moderator
workspace-settings-role-editor = Editor
workspace-settings-role-commenter = Commenter
workspace-settings-role-viewer = Viewer
workspace-settings-role-blocked = Blocked
workspace-settings-role-admin = Admin
workspace-settings-role-guest = Guest
workspace-settings-role-owner-preview = Can manage people, permissions, and workspace settings.
workspace-settings-role-moderator-preview = Can edit shared work, remove other people's messages, lock channels, and remove or ban editors and below.
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
workspace-settings-subject-bot = Bot { $id }
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
workspace-settings-ownership-cancel-short = Cancel transfer
workspace-settings-ownership-transfer = Transfer ownership
workspace-settings-you-manage = You manage
workspace-settings-view-only = View only
workspace-settings-members-empty-title = No one else is in this workspace
workspace-settings-members-empty-detail = Create a signed invite from the General tab to add someone.
workspace-settings-people-header = People
workspace-settings-ownership-offered = { $owner } offered you ownership. Review the workspace and explicitly accept before the offer expires.
workspace-settings-ownership-waiting = Waiting for { $recipient } to accept ownership.
workspace-settings-ownership-pending = Ownership transfer to { $recipient } is awaiting that person's acceptance.
workspace-settings-you = You
workspace-settings-member-extra = { $count ->
    [one] 1 extra permission
   *[other] { $count } extra permissions
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
workspace-settings-bots-header = Bots
workspace-settings-add-bot = Add a bot
workspace-settings-bots-empty = No bots run in this workspace
workspace-settings-bot-tag = Bot
workspace-settings-bot-starting = Starting
workspace-settings-bot-paused = Paused
workspace-settings-bot-running = Running
workspace-settings-bot-version = Version { $version }
workspace-settings-remove-bot = Remove
workspace-settings-agents-header = Agents
workspace-settings-service-member = { $access ->
    [admin] Administers
    [write] Reads and edits
    [comment] Reads and comments
   *[read] Reads
} encrypted content · { $until }
workspace-settings-service-until-removed = until removed
workspace-settings-service-ends-soon = access ends within a minute
workspace-settings-service-ends-minutes = access ends in { $minutes } min
workspace-settings-service-ends-hours = access ends in { $hours } h
workspace-settings-badge-service = Service
workspace-settings-badge-agent = Agent
workspace-settings-agent-detail = { $count ->
    [0] { $admission } · No permissions yet, review needed
    [one] { $admission } · { $profile }, 1 permission
   *[other] { $admission } · { $profile }, { $count } permissions
}
workspace-settings-admitted = Admitted
workspace-settings-review-access = Review access
workspace-settings-agent-access-back = All agents
workspace-settings-agent-access-note = Choose what this agent may do in each app. Sending, publishing and deleting for good still ask you first, unless you chose Always allow for that action.
workspace-settings-agent-access-read = Reading
workspace-settings-agent-access-change = Changes
workspace-settings-agent-access-outside = Outside effects
workspace-settings-agent-access-summary-part = { $kind }: { $answer }
workspace-settings-agent-access-custom = Custom
workspace-settings-agent-access-custom-detail = Set for this agent
workspace-settings-agent-access-inherited = Follows its admission
workspace-settings-agent-access-set-here = set here
workspace-settings-agent-access-default = default
workspace-settings-agent-access-reset = Use defaults
workspace-settings-agent-access-always = Always allowed · { $count }
workspace-settings-agent-access-stop = Ask again
workspace-settings-agent-model = Model
workspace-settings-agent-model-default = The default: { $model }
workspace-settings-agent-model-none = No model chosen in Settings
workspace-settings-agent-model-use = Use
workspace-settings-agent-model-in-use = In use
workspace-settings-agent-access-empty = No apps in this workspace yet.
workspace-settings-answer-allow = Allow
workspace-settings-answer-ask = Ask first
workspace-settings-answer-deny = Deny
workspace-settings-revoke = Revoke
workspace-settings-admission-admin = Admin admission
workspace-settings-admission-write = Write admission
workspace-settings-admission-comment = Comment admission
workspace-settings-admission-read = Read admission
workspace-settings-unavailable-app = Unavailable app
workspace-settings-apps-empty-title = No file handlers installed
workspace-settings-apps-empty-detail = No app declares a supported file type yet.
workspace-settings-apps-intro-title = Defaults without lock-in
workspace-settings-apps-intro-detail = The workspace supplies a baseline; each person can explicitly choose something else.
workspace-settings-apps-header = Default apps
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
workspace-settings-who-agent = Who: an agent, with access to this window only
workspace-settings-nobody-to-grant = Nobody to grant to yet. Invite someone, or open an app here.
workspace-settings-what-it-allows = What it allows
workspace-settings-grant-header = Grant a permission
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
workspace-settings-current-workspace = Current workspace
workspace-settings-identity-title = Workspace identity
workspace-settings-identity-description = This name appears in the top bar, invites, and shared activity.
workspace-settings-rename-admin-only = Only a workspace administrator can rename this workspace.
workspace-settings-invite-admin-only = Only an administrator of this workspace can invite people.
workspace-settings-invite-create = Create invite
workspace-settings-devices-title = Your devices
workspace-settings-devices-local = Your window layout and controls stay on each device.
workspace-settings-device-here = { $device } (this device)
workspace-settings-device-shown = Shown
workspace-settings-device-hidden = Kept off
workspace-settings-move-earlier = Move up
workspace-settings-move-later = Move down
workspace-settings-delete = Delete workspace
workspace-settings-delete-description = Removes it from this device and your other devices.
workspace-settings-delete-action = Delete
workspace-settings-archived-title = Archived workspaces
workspace-settings-archived-restore = Restore
workspace-settings-archived-delete = Delete
workspace-settings-public-title = Public
workspace-settings-public-asking = Asking this workspace's host…
workspace-settings-public-on = Anyone can find it in its host's directory and join.
workspace-settings-public-off = Only people you invite can join. Make it public to list it in its host's directory.
workspace-settings-public-switch-on = Public
workspace-settings-public-switch-off = Private
workspace-settings-public-role = People who join: { $role }
workspace-settings-public-describe = Description…
workspace-settings-invite-title = Invite people
workspace-settings-invite-description = Choose a person and what they can do here. Each invite works for one hour.
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
a11y-shell-people-description = Who is here, where they are and what they are doing
a11y-shell-people-here-now = People here now
a11y-shell-people-agent = Agent
a11y-shell-people-now = Now: { $activity }
a11y-shell-people-recent-toggle = What { $name } did recently
a11y-shell-people-recent-toggle-description = Show or hide what they changed recently
a11y-shell-rename = Change your name, { $name }
a11y-shell-rename-description = Choose the name others here see
a11y-shell-sharing-change = Change what you share
a11y-shell-follow-collaborator = Follow collaborator
a11y-shell-follow = Follow { $name }
a11y-shell-follow-description = Mirror this participant's live workspace view
a11y-shell-invite = Invite people
a11y-shell-invite-description = Create an authenticated workspace invitation
a11y-shell-join = Join workspace
a11y-shell-join-description = Connect using an authenticated invitation
a11y-shell-workspace-activity-description = Review grouped outcomes and what happened while you were away

# Empty-workspace launchpad
launchpad-title = Make something together
launchpad-title-private = Start something
launchpad-description = Choose a conversation, notes, tasks, a call, an invitation, or workspace activity.
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
launchpad-discuss-detail-private = Open this workspace's conversation
launchpad-notes-detail-private = Write ideas down
launchpad-meet-detail-private = Call someone from Chat
launchpad-invite-detail-private = Work on this with someone

# Empty-workspace launchpad: the keyboard hint under the cards
launchpad-keyboard-hint = Arrow keys move, Enter opens, Super+N shows every app

# Peers arriving in and leaving a workspace, batched into one toast

# Moved from the shell catalogue with the code that uses them
people-panel-agent-tag = Agent
people-panel-bot-tag = Bot
people-panel-viewing = Viewing { $title }
people-panel-in-app = In { $app }
people-panel-here-count = { $count ->
    [0] Just you
    [one] You and 1 other
   *[other] You and { $count } others
}
people-panel-here-watching = { $count ->
    [0] Just you
    [one] You and 1 other
   *[other] You and { $count } others
}, { $watching } watching
people-panel-here-spectating = { $count ->
    [0] Nobody here
    [one] 1 person here
   *[other] { $count } people here
}, { $watching } watching, you among them
people-panel-section-people = People
people-panel-section-agents = Agents
people-panel-section-devices = Your devices
people-panel-wave = Wave
people-panel-waved = Waved
people-panel-join = Join
people-panel-in-call = In { $channel }
people-panel-in-a-call = In a call
people-panel-focusing = Focusing
people-panel-away = Away
people-panel-idle = Idle
people-panel-empty = Nobody else is here right now
people-panel-changes = Their changes
people-panel-invite-people = Invite people
a11y-shell-wave = Wave at { $name }
a11y-shell-waved = You waved at { $name }
a11y-shell-wave-description = Their face shows you waved
a11y-shell-join-call = Join { $name }’s call
a11y-shell-join-call-description = Joins the voice channel they are in
a11y-shell-people-changes = What { $name } changed
a11y-shell-people-changes-description = Open History at their changes
a11y-shell-people-change-description = Open History at this change
people-panel-follow = Follow
people-panel-stop-following = Stop

# The people overlay: the roster pinned out of the bar, floating over windows
people-panel-pin = Keep on screen
a11y-people-pin = Keep who is here on screen
people-overlay-title = { $count ->
    [0] Just you
    [one] 1 here
   *[other] { $count } here
}
people-overlay-unpin = Put back in the bar
people-overlay-follow-name = Follow { $name }
people-overlay-doing-nothing = Here
people-overlay-screen-private = { $name } keeps where they are to themselves
people-overlay-empty-desk = Nothing open on { $name }’s screen
people-overlay-message-placeholder = Message { $name }
people-overlay-send = Send
people-overlay-you = You
people-overlay-no-messages = Say hello to { $name }
people-overlay-messages-unavailable = Direct messages need the Tilo app on this device
people-overlay-message-failed = The message to { $name } did not go: { $reason }
a11y-people-follow = Follow { $name }
a11y-people-stop-following = Stop following { $name }
launchpad-subtitle = Pick a way to start. Everyone in this workspace sees what you make here.
launchpad-subtitle-private = Pick a way to start. Only you see what you make here until you invite someone.
launchpad-subtitle-public = Pick a way to start. This workspace is public: anyone who joins sees what you make here.

# The emoji picker (Super+. by default)
emoji-picker-title = Emoji
emoji-picker-search = Search emoji
emoji-picker-groups = Emoji categories
emoji-picker-recent = Recently used
emoji-picker-results = Matching emoji
emoji-picker-no-results = No emoji found
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
window-switcher-caption-paged = { $position } of { $count }

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

# The launchpad in the browser: a new member's first look at the desktop
browser-welcome-title = Welcome to Tilo
browser-welcome-subtitle = Your team's shared computer. Everything you make is saved as you go and end-to-end encrypted.
browser-welcome-hint = Arrow keys move, Enter opens, Ctrl+K finds any app
browser-welcome-hint-mac = Arrow keys move, Enter opens, ⌘K finds any app

# The overview panel a host without its own overview draws
overview-panel-title = Overview
overview-panel-you-are-here = You are here
overview-panel-windows = Windows here
overview-strip-empty = No windows
overview-panel-no-windows = No windows yet
overview-panel-window-count = { $count ->
    [one] 1 window
   *[other] { $count } windows
}
overview-panel-go-to = Go to
overview-panel-new-workspace = New workspace
browser-new-workspace-name = Workspace { $number }
room-notice-joined = { $name } joined
room-notice-renamed = { $name } renamed the workspace to { $workspace }
browser-app-crashed = { $app } stopped working, so it was closed to keep the rest of the desktop running. Close this window, or open { $app } again.
browser-app-crashed-reopen = Reopen

# The menu behind the top bar's Tilo mark, and the About sheet it opens
tilo-menu-about = About this computer
tilo-menu-about-browser = About Tilo
tilo-menu-feedback = Give feedback…
tilo-about-title = About this computer
tilo-about-title-browser = About Tilo
tilo-about-lead = This is Tilo, an operating system.
tilo-about-body-browser = You're using Tilo in your browser. It's coming to run on your own computer, too.
tilo-about-version = Version { $version }
tilo-about-link-get = Get Tilo
tilo-about-link-home = tilo.so
tilo-about-body-native = It runs right here, on this computer.
tilo-about-link-notify = Get notified
tilo-about-link-source = Source code
tilo-about-link-community = Community

notification-preview-hidden = Notification preview hidden
activity-acknowledge-viewed = Mark as seen
activity-acknowledge-description = Marks the changes shown here as seen. Waiting tasks stay open.
notification-alarm-snoozed = Snoozed for 5 minutes. Dismiss this entry to cancel the pending snooze.
notification-alarm-dismiss = Dismiss
notification-alarm-snooze = Snooze for 5 minutes
notification-alarm-action-failed = Could not change this alarm
notification-alarm-action-title = { $action }: { $title }
notification-alarm-action-subtitle = For this alarm occurrence
notification-alarm-action-retry = Keep this notification and try again.

activity-morning-choose = Choose for morning
activity-morning-remove = Remove morning source
activity-morning-empty = Morning sources: choose a resource in History, then Details. Up to eight sources; none are connected automatically.
activity-morning-description = Chosen morning sources: provider previews from authorized workspace resources. Refresh or draft a schedule explicitly.
activity-morning-unavailable = This source isn't available here
activity-morning-source-unnamed = Chosen source
activity-morning-source = { $source }, last activity { $time } · may be out of date

activity-morning-open = Open morning source

activity-morning-payload-unavailable = Provider summary unavailable. Open the source to inspect or refresh it.

activity-morning-refresh = Refresh source
activity-morning-draft = Draft scheduled refresh…
activity-morning-draft-description = Opens a draft in Shortcuts. Choose a schedule and review its permissions before saving; nothing is armed yet.
activity-morning-command-unavailable = Source command unavailable
activity-morning-command-pending = Request pending
activity-morning-command-accepted = Request accepted; inspect its result in Actions
activity-morning-command-refused = Request refused
activity-morning-recorded = { $source } · last recorded workspace activity: { $time }

activity-morning-next = Next chosen source

emoji-picker-hint-tone = Pick a skin tone

workspace-settings-hint-close = Close
workspace-settings-hint-save = Save
workspace-settings-hint-cancel = Cancel
workspace-settings-hint-choose = Choose

workspace-settings-hint-apply = Apply

# Someone else opened a window out of sight: the hint at the strip's edge
desktop-window-opened-elsewhere = { $person } opened { $app }
# Strip switcher: the strip ribbon grown into labels, one per strip of the workspace.
strip-switcher-a11y = Strips
strip-switcher-unnamed = Strip { $number }
strip-switcher-new-strip = New strip
strip-switcher-rename-named = Rename { $name }
strip-switcher-name-placeholder = Strip name
strip-switcher-refused = Couldn't change the strip
strip-switcher-remove-named = Remove { $name }
strip-switcher-rename = Rename
strip-switcher-remove = Remove

## Reporting a problem by pointing at it
feedback-report-problem = Report a problem…
feedback-picker-hint = Click what isn’t working. Shift-click or drag to pick more, scroll to widen, Esc to leave.
feedback-picker-picked = { $count ->
    [one] 1 part picked. Shift-click or drag to pick more.
   *[other] { $count } parts picked. Shift-click or drag to pick more.
}
feedback-picker-continue = Continue
feedback-picker-cancel = Cancel

# Following someone's view
follow-label = Following { $person }
follow-stopped = Stopped following
follow-stop = Stop
