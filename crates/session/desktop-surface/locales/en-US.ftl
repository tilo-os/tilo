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
# A running application's icon in the bar, when the item that named it is gone.
topbar-app-tray-item = Running application
topbar-app-tray-unread = { $count } unread
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
topbar-show-collaborators = Everyone here
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
workspace-menu-workspace = Workspace
workspace-menu-delete = Delete workspace
profile-card = Your profile
profile-card-of = { $name }’s profile
profile-id = Person ID
profile-copy-id = Copy ID
profile-copy-device-key = Copy this device’s key
profile-device-this = This computer
profile-device-another = Another device
profile-device-mark = this device
profile-status-own-words = In their own words
profile-status-automatic = Written automatically from what they do
profile-message = Message
profile-follow = Follow
profile-stop-following = Stop following
profile-photo-change = Change picture
profile-photo-remove = Remove picture
profile-name = Your name
profile-name-placeholder = Your name
profile-name-hint = How everyone here sees you
profile-add-your-name = Add your name
profile-section-identity = Your identity
profile-section-devices = Devices
profile-section-linked = Linked accounts
profile-link = Connect { $service }
profile-unlink = Unlink
profile-linked-as = Linked as { $account }
profile-linked-to = Linked to { $handle } on { $service }
profile-link-offer = Link { $service } { $account } to { $name }?
profile-link-offer-why = What you write in { $service } will show as you here, and as you there.
profile-link-accept = Link
profile-link-decline = Cancel
profile-connecting = Opening { $service }…
profile-looking = Checking your { $service } link…
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
profile-control-hints = Show control hints
control-hint-open = Open anything
control-hint-move = Drag to move
control-hint-resize = Drag to resize
control-hint-pan = Scroll sideways
control-hint-pan-touch = Swipe left or right

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
activity-detail-undo-run = Undo this whole run
activity-detail-split = Split
activity-detail-merge = Merge
activity-detail-action-count = { $count } { $count ->
        [one] action
       *[other] actions
    }
activity-action-mark-viewed = Mark as viewed
activity-action-focused = Say what was shown
activity-action-name = Name changes
activity-action-move-boundary = Move work session boundary
activity-action-undo-record = Undo this change
activity-action-choose-source = Choose morning source
activity-action-source = Use morning source
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
activity-undo-run-needs-open = Open every document this run changed to undo it together.
activity-undo-run-in-progress = Undoing this run is still under way.
activity-undo-run-nothing-to-undo = This run changed no document that keeps its own history.
activity-undo-task-needs-documents = Nothing changed. Open every affected document first.
activity-undo-task-partial = Task partly undone; later collaborator edits were kept.
activity-undo-task-partial-records = Task undone, except for records listed below.
activity-undo-task-record-left = { $record } in { $app } was not undone.
activity-detail-undo-step = Undo this step
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
desktop-search-copied = Copied
desktop-search-searching = Searching…
desktop-view-switch-workspace = Switch to { $workspace }
desktop-view-review-activity = Review workspace activity
desktop-view-explore-activity = Explore workspace activity
desktop-view-replay-activity = Replay workspace activity

## Desktop window search
search-window-nth = { $nth } of { $count }
search-window-placeholder = Search { $searched }…

# Notifications: banner stack and notification centre
drill-in-back = Back
a11y-notifications-none = No retained notifications.
a11y-notifications-description = { $count ->
    [one] { $count } retained notification, grouped by app.
   *[other] { $count } retained notifications, grouped by app.
} Use Up and Down to select a notification, Page Up and Page Down to move a screen, and Home or End to jump to the newest or oldest one. Enter opens the selected notification or folds its app group, and Delete dismisses it.
a11y-notifications-list = Retained notifications
a11y-notifications-clear = Clear all notifications
a11y-notifications-clear-description = Delete this session's retained notification history. An Undo action remains available for eight seconds.
a11y-notifications-cleared = Notifications cleared
a11y-notifications-undo = Undo clear notifications
a11y-notifications-undo-description = Restore the cleared notifications in their original order. Press Enter or Space.
a11y-notifications = Notifications
notification-center-title = Notifications
notification-center-empty = No recent notifications
notification-center-cleared = Notifications cleared
notification-center-clear-all = Clear all
notification-center-close = Close notifications
notification-center-back-to-list = Back to notifications
history-severity-info = Information
history-severity-warning = Warning
history-severity-error = Error
history-notification-description-sourced = { $severity }. From { $source }. { $time }
history-notification-description-sourced-body = { $severity }. From { $source }. { $time }. { $body }
host-action-source-identity = Identity
host-action-source-devices = Devices
host-action-source-workspace = Workspace
host-action-source-session = This computer
notification-center-group-description = { $count } notifications. Press Enter to { $expanded ->
        [yes] collapse
       *[no] expand
    }, Delete to dismiss them all.
notification-center-dismiss-all = Dismiss all from { $app }
notification-reply = Reply
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
notification-more-from = +{ $count } more from { $app }
notification-stack-a11y = Notifications
notification-group-system = System
notification-center-undo = Undo
notification-not-now = Not now
notification-app-failed-title = Couldn't open an app
notification-service-unavailable = This isn't available on this desktop yet

# Your devices: pairing a new one from another of yours, and administering
# your identities, as Settings asks the session for them.
search-action-identity-resource = Your identities
search-action-identity-list = List your identities
search-action-identity-list-subtitle = Show the identities on this device and which one new apps sign in as
search-action-identity-activate = Use an identity for new apps
search-action-identity-activate-subtitle = Sign apps you open from now on in as one of your identities
search-action-identity-remove = Remove an identity from this device
search-action-identity-remove-subtitle = Delete an identity you no longer use here, with its key
search-action-identity-protect = Protect an identity's key
search-action-identity-protect-subtitle = Move an identity's key from a plain file into protected storage
search-action-identity-devices-list = List your trusted devices
search-action-identity-devices-list-subtitle = Show the devices trusted under this workspace's identity and which are revoked
search-action-identity-device-revoke = Revoke an identity device
search-action-identity-device-revoke-subtitle = Permanently stop trusting a lost, retired, or compromised device
search-action-identity-home-peer-remove = Remove a home server
search-action-identity-home-peer-remove-subtitle = Revoke a paired home server and stop it serving your workspaces
search-action-identity-home-peer-retry = Finish removing home servers
search-action-identity-home-peer-retry-subtitle = Tell removed home servers that could not be reached before to stop
search-action-identity-device-pair-resource = Nearby device pairing
search-action-identity-device-pair-discover = Find a nearby device to pair
search-action-identity-device-pair-discover-subtitle = Look on this network for a new device or home server waiting to join
search-action-identity-device-pair-approve = Approve a nearby device
search-action-identity-device-pair-approve-subtitle = Let the device found nearby join once both screens show the same code
search-action-identity-device-pair-reject = Reject a nearby device
search-action-identity-device-pair-reject-subtitle = Forget a device found nearby and stop approving it
search-action-identity-device-pair-asking = Devices asking to be you
search-action-identity-device-pair-asking-subtitle = The new devices your other devices found, waiting for you to approve them here
search-action-identity-device-pair-answer = Answer a device asking to be you
search-action-identity-device-pair-answer-subtitle = Approve or decline a device another of your devices found, as its notification does

# A new device another of yours found: the banner that asks the person here.
pairing-asks-title = A device asks to be you
pairing-asks-body = { $device }{ $from }, { $when }. Approving lets it act as you. Only approve if it shows { $code }.
pairing-asks-via = {" "}(found by your { $device })
pairing-approve = Approve { $code }
pairing-decline = Decline

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
workspace-settings-title = Workspace Settings
workspace-settings-close = Close Settings
workspace-settings-tab-general = General
workspace-settings-tab-people = People
workspace-settings-tab-agents = Agents
workspace-settings-tab-privacy-security = Privacy & Security
workspace-settings-tab-privacy = Privacy
workspace-settings-tab-apps-files = Apps & Files
workspace-settings-tab-apps = Apps
workspace-settings-tab-emoji = Emoji
workspace-settings-emoji-count = { $count ->
    [0] This workspace's own emoji
    [one] 1 emoji of this workspace
   *[other] { $count } emoji of this workspace
}
workspace-settings-emoji-help = Write :name: in any app of this workspace, or pick one from a reaction's emoji.
workspace-settings-emoji-add = Add emoji
workspace-settings-emoji-empty-title = This workspace has no emoji of its own yet
workspace-settings-emoji-empty-detail = Choose Add emoji and pick a picture: its file name becomes the emoji's name.
workspace-settings-emoji-empty-detail-viewer = Anyone who may edit this workspace can add them.
workspace-settings-emoji-unavailable = This workspace's emoji are not available on this device.
workspace-settings-emoji-editors-add = Only people who may edit this workspace can add emoji.
workspace-settings-emoji-added-by = Added by { $name }
workspace-settings-emoji-added-by-you = Added by you
workspace-settings-emoji-remove = Remove :{ $name }:
workspace-settings-emoji-not-chosen = No picture was chosen
workspace-settings-emoji-not-added = { $file } was not added as an emoji
workspace-settings-emoji-not-removed = :{ $name }: was not removed
workspace-settings-emoji-tools-loading = The picture tools are still loading. Try again in a moment.
workspace-settings-emoji-not-a-picture = Only PNG, JPEG and WebP pictures can become emoji.
workspace-settings-emoji-too-detailed = The picture is too detailed to stay sharp at emoji size. Try a simpler one.
workspace-settings-emoji-no-name = Its file name has no letters or digits to name an emoji by.
workspace-settings-emoji-too-large = The picture is too large to become an emoji. Choose one under 8 MB.
workspace-settings-emoji-unreadable = The picture could not be read.
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
workspace-settings-members-count = { $count ->
    [one] 1 person shares this workspace
   *[other] { $count } people share this workspace
}
workspace-settings-members-help = Removing someone blocks future sync and edits; copies already downloaded stay with them.
workspace-settings-you-manage = You manage
workspace-settings-view-only = View only
workspace-settings-members-empty-title = No one else is in this workspace
workspace-settings-members-empty-detail = Create a signed invite from the General tab to add someone.
workspace-settings-people-header = People
workspace-settings-you = You
workspace-settings-member-extra = { $count ->
    [one] 1 extra permission
   *[other] { $count } extra permissions
}
workspace-settings-remove = Remove
workspace-settings-owner-only = Only the workspace owner can change permissions.
workspace-settings-refused = The workspace refused that change
workspace-settings-refused-own-role = Nobody changes their own role.
workspace-settings-refused-own-membership = Nobody removes themselves from the workspace.
workspace-settings-agents-count = { $count ->
    [one] 1 agent
   *[other] { $count } agents
}
workspace-settings-agents-help = Agents work alongside you here. They only do what you allow.
workspace-settings-add-agent = New agent
workspace-settings-create-agent = Create
workspace-settings-cancel = Cancel
workspace-settings-agents-empty-title = No agents yet
workspace-settings-agents-empty-detail = An agent works alongside you here, with the access you allow it.
workspace-settings-bots-header = Bots
workspace-settings-bots-help = Bots run small programs here on their own: on a schedule, when something changes, or as a bridge to a service like Discord.
workspace-settings-add-bot = Add a bot
workspace-settings-bots-empty = No bots run in this workspace
workspace-settings-bot-tag = Bot
workspace-settings-bot-starting = Starting
workspace-settings-bot-paused = Paused
workspace-settings-bot-running = Running
workspace-settings-bot-version = Version { $version }
workspace-settings-remove-bot = Remove
workspace-settings-agents-header = Agents
workspace-settings-retire-agent = Remove
workspace-settings-agent-name = Name
workspace-settings-agent-colour = Colour
workspace-settings-agent-colour-hint = Its colour comes from its name until you pick one.
workspace-settings-agent-name-number = Agent { $number }
workspace-settings-agent-yours = Yours
workspace-settings-agent-keeper-you = Kept by you
workspace-settings-agent-keeper = Kept by { $name }
workspace-settings-agent-model-line = Model: { $model }
workspace-settings-agents-unavailable = The workspace's agents cannot be read on this device
workspace-settings-agent-name-needed = Name the agent first
workspace-settings-review-access = Review access
workspace-settings-agent-access-back = All agents
workspace-settings-agent-access-note = Choose what this agent may do in each app. Sending, publishing and deleting for good still ask you first, unless you chose Always allow for that action.
workspace-settings-agent-access-read = Reading
workspace-settings-agent-access-change = Changes
workspace-settings-agent-access-outside = Outside effects
workspace-settings-agent-access-set-here = Set here
workspace-settings-agent-access-reset = Use defaults
workspace-settings-agent-access-always = Always allowed · { $count }
workspace-settings-agent-exception = { $action } in { $app }
workspace-settings-agent-access-stop = Ask again
workspace-settings-agent-model = Model
workspace-settings-agent-model-default = The default: { $model }
workspace-settings-agent-model-none = No model chosen in Settings
workspace-settings-agent-model-default-choice = Your default model
workspace-settings-agent-model-use = Use
workspace-settings-agent-model-option = { $model } from { $provider }
workspace-settings-agent-model-in-use = In use
workspace-settings-agent-model-keeper = Only { $name } chooses the model it runs on their devices
workspace-settings-agent-model-keeper-you = You choose the model it runs on your devices
workspace-settings-agent-spend-of = { $spent } of { $limit } today
workspace-settings-agent-spend = { $spent } today
workspace-settings-agent-limit = Daily limit
workspace-settings-agent-limit-amount = { $limit } a day
workspace-settings-agent-limit-none = No limit
workspace-settings-agent-limit-today = { $spent } today
workspace-settings-agent-limit-keeper-you = Only you set what it may spend a day. It stops there and tells you when a run reaches it.
workspace-settings-agent-limit-keeper = Only { $name } sets what it may spend a day.
workspace-settings-agent-limit-placeholder = Dollars a day
workspace-settings-agent-limit-set = Set
workspace-settings-agent-limit-not-amount = A limit is an amount of dollars, like 5 or 0.50
workspace-settings-agent-access-empty = No apps in this workspace yet.
workspace-settings-answer-allow = Allow
workspace-settings-answer-ask = Ask first
workspace-settings-answer-deny = Deny
workspace-settings-revoke = Revoke
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
workspace-settings-name-label = Workspace name
workspace-settings-rename-admin-only = Only a workspace administrator can rename this workspace.
workspace-settings-invite-not-allowed = Only someone this workspace allows to invite people can make a link.
workspace-settings-invite-create = Create invite
workspace-settings-devices-title = Your devices
workspace-settings-devices-local = The workspace's windows, groups and strips are shared with everyone in it. Choose which of your devices show it, and where it sits on this one.
workspace-settings-device-here = { $device } (this device)
workspace-settings-device-this = This device
workspace-settings-device-unnamed = One of your devices
workspace-settings-device-shown = Shown
workspace-settings-device-hidden = Kept off
workspace-settings-move-earlier = Move up
workspace-settings-move-later = Move down
workspace-settings-leave = Leave workspace
workspace-settings-leave-description = Takes it off this device, along with the windows and groups that belong to it. The workspace stays for everyone in it.
workspace-settings-leave-action = Leave
workspace-settings-leave-sole-member = You are the only member here, so there is nobody for it to stay with.
workspace-settings-leave-last = This is your only workspace here.
workspace-settings-delete = Delete workspace
workspace-settings-delete-description = Everything in it goes: its windows, its groups, its history. It disappears from every device that has it.
workspace-settings-delete-action = Delete
workspace-settings-delete-not-held = This device does not keep it.
workspace-settings-delete-others = Other people are in it.
workspace-settings-delete-elsewhere = Another of your devices has it: delete it there.
workspace-settings-delete-not-owner = It belongs to someone else.
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
a11y-shell-stop-following = Stop following { $name }
a11y-shell-invite = Invite people
a11y-shell-invite-description = Create an authenticated workspace invitation
a11y-shell-join = Join workspace
a11y-shell-join-description = Connect using an authenticated invitation
a11y-shell-workspace-activity-description = Review grouped outcomes and what happened while you were away

# Empty-workspace launchpad
launchpad-title = Start something
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
launchpad-tasks-detail = Write down what needs doing
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
people-panel-unpin = Put back in the bar
a11y-people-unpin = Put who is here back in the bar
people-panel-screen-private = Keeps their place private
people-panel-nothing-open = Nothing open on their screen
people-panel-pin = Keep on screen
a11y-people-pin = Keep who is here on screen
a11y-people-overlay = Who is here
a11y-people-follow = Follow { $name }
a11y-people-stop-following = Stop following { $name }
launchpad-subtitle = Everyone in this workspace sees what you make here.
launchpad-subtitle-private = Only you see what you make here until you invite someone.
launchpad-subtitle-public = This workspace is public: anyone who joins sees what you make here.

# The Alt+Tab window switcher.
window-switcher-title = Switch windows
window-switcher-description = Windows in this workspace, most recently used first. Hold Alt and press Tab or the arrow keys to move, release Alt to switch, or press Escape to stay.
window-switcher-app-title = Switch windows of this app
window-switcher-app-description = This app's windows in this workspace, most recently used first. Hold Alt and press ` to move, release Alt to switch, or press Escape to stay.
window-switcher-untitled = Untitled window
window-switcher-caption-paged = { $position } of { $count }
window-switcher-hint-walk = Walk
window-switcher-hint-go = Go there
window-switcher-hint-stay = Stay

# Workspace overview
overview-empty-window = Empty window
overview-untitled = Untitled
overview-access-only-you = Only you
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
browser-welcome-hint = Arrow keys move, Enter opens

# The workspace overview every desktop shows
overview-title = Workspace overview
overview-strip-empty = No windows
overview-go-to = Go to
overview-no-workspace = No workspace selected
overview-you-are-here = You are here
overview-shared-out = Shared out of here
overview-more-shares = and { $count } more
overview-share-with-two = { $first } and { $second }
overview-share-with-many = { $count ->
        [one] { $first } and { $count } other
       *[other] { $first } and { $count } others
    }
overview-share-can-edit = { $with } can edit
overview-new-workspace = New workspace
overview-join-workspace = Join a workspace…
overview-leave-workspace = Leave
overview-a11y-leave = Leave { $name }
overview-two-facts = { $first }, { $second }
overview-tilo-more-workspaces = More workspaces
overview-tilo-hours = { $hours } hr
overview-tilo-hosts = Hosts & compute
overview-tilo-agents = Agents
overview-tilo-account = Account
overview-tilo-about = About Tilo
overview-tilo-workspace-usage = { $host }, { $time } this month
overview-tilo-plan-used-of = { $used } of { $included } hr
overview-tilo-plan-used = { $used }
overview-tilo-credit = { $amount } credit
overview-tilo-no-card = No card yet: added the first time something needs a Linux machine
overview-tilo-no-plan = No plan chosen
overview-tilo-host-device = This device
overview-tilo-host-self = Your own host
overview-tilo-host-provider = Hosting provider
overview-tilo-manage-billing = Manage billing
overview-tilo-agent-everywhere = All your workspaces
overview-tilo-agent-everywhere-lower = all your workspaces
overview-tilo-agent-browser-pages = Operates web pages in Browser
overview-tilo-agent-working = { $runs ->
        [one] Working now
       *[other] { $runs } runs now
    }
overview-tilo-revoke = Revoke
overview-tilo-revoke-question = { $agent } loses access to { $reach } and stops now
overview-tilo-revoke-access = Revoke access
overview-tilo-keep = Keep
overview-windows-title = Windows in this workspace
overview-windows-description = Each strip of this workspace, top to bottom, with its windows in order. The arrow keys move between them and Enter goes to one. F2 renames the strip a window is on, and Alt with Up or Down moves that strip. Click or tap a window to go there, middle-click one to close it, or drag it onto another workspace or strip to move it there.
overview-strips-title = Strips in this workspace
overview-strip-rename = Rename
overview-strip-move-up = Move up
overview-strip-move-down = Move down
browser-new-workspace-name = Workspace { $number }
workspace-notice-joined = { $name } joined
workspace-notice-agent-finished = { $agent } finished
workspace-notice-agent-card = Open card
workspace-notice-agent-finished-changes = { $agent } finished: { $count ->
    [one] 1 change
   *[other] { $count } changes
}
workspace-notice-agent-finished-in = { $agent } finished: { $count ->
    [one] 1 change
   *[other] { $count } changes
} in { $workspace }
workspace-notice-renamed = { $name } renamed the workspace to { $workspace }
# What the desk says when something a person asked for did not happen: what
# they asked for, why, and the button that tries it again.
answer-app-not-opened = { $app } could not open
answer-app-stopped = { $app } stopped working
answer-why-stopped = It ended unexpectedly.
answer-app-closed-for-memory = { $app } was closed to free memory
answer-app-closed-for-runaway = { $app } was using too much memory
answer-why-runaway = It was closed so the rest keeps running.
answer-try-again = Try again
answer-reopen = Reopen
answer-why-unknown-app = No app here opens it.
answer-why-not-opened = It was stopped before a window appeared.
answer-why-cannot-start = Apps cannot be started right now.
# The row named here is the one the window's menu offers on the device that has the window.
peer-window-elsewhere = Open on another device. Choose “{ window-menu-share-with-workspace }” in the window's menu there to see it on this one.
peer-window-on-device = Open on { $device }. Choose “{ window-menu-share-with-workspace }” in the window's menu there to see it on this one.
# A chord of the desktop's a browser page has no surface for: it says so,
# rather than doing nothing.
answer-capture-not-in-browser = Screenshots and recordings run in Tilo on a computer
answer-why-capture-not-in-browser = It needs the whole screen, which a browser page does not have.
answer-remote-not-in-browser = Remote control runs in Tilo on a computer
answer-why-remote-not-in-browser = Nothing here is under someone else's control.

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
notification-alarm-dismiss = Dismiss
notification-alarm-snooze = Snooze for 5 minutes
notification-sender-claims = App says: { $name }
notification-open = Open
notification-quiet-do-not-disturb = Do Not Disturb
notification-quiet-presenting = Do Not Disturb while presenting
notification-quiet-sharing = Do Not Disturb while sharing
notification-quiet-focus-until = Focus until { $time }

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
# What a picked part is called while picking and in the review: what it is,
# then in which app ("Message in Chat", "The “Send message” button in Chat").
feedback-part-in = { $what } in { $app }
# A part the app names with its own words, by its role.
feedback-part-named = The “{ $name }” { $role ->
    [Button] button
    [ToggleButton] button
    [Link] link
    [Tab] tab
    [MenuItem] menu item
    [Checkbox] checkbox
    [Switch] switch
    [TextInput] field
    [MultilineTextInput] field
    [Heading] heading
   *[other] { $role }
}
# A part known only by its role, as assistive technology names it.
feedback-part-role = { $role ->
    [ListItem] A list item
    [Row] A row
    [Cell] A cell
    [Button] A button
    [ToggleButton] A button
    [Link] A link
    [Image] A picture
    [TextInput] A field
    [MultilineTextInput] A field
    [StaticText] A text
    [Paragraph] A paragraph
    [Heading] A heading
    [List] A list
    [Group] A group
    [Window] A window
    [Toolbar] A toolbar
    [Menu] A menu
    [MenuItem] A menu item
    [Tab] A tab
   *[other] A part
}

# Following someone's view
follow-label = Following { $person }
follow-stopped = Stopped following
follow-stop = Stop

people-follows = Following { $person }
people-follows-you = Following you
people-follows-where = { $place }, following { $person }
people-follows-you-where = { $place }, following you

# The way out of a window that filled the display, said over it once.
fullscreen-way-out = Press { $key } to leave full screen.

## The desktop's published actions
desktop-actions-chrome = Desktop
desktop-actions-desk = Windows
desktop-actions-launchpad = Launchpad
desk-focus-window = Focus { $window }
desk-close-window = Close { $window }
desk-look-window = See { $window } as a picture
desk-read-window = Read what { $window } shows
session-host-new-workspace = New workspace
session-host-new-workspace-description = Make one on this computer
session-host-branch-workspace = Branch workspace
session-host-branch-workspace-description = A copy of this one that goes its own way
desktop-branch-workspace-name = { $name } branch
desktop-cannot-branch-workspace = This workspace holds what you can't read, so it can't be branched here
session-branching-workspace = Branching { $name }
session-branching-workspace-body = { $branch } opens once its records, files and environments are copied.
session-branched-workspace = { $branch } is ready
session-branched-workspace-body = It has what { $name } has, with you as its only member. Its chat stayed behind.
session-branch-failed = { $branch } is not complete
session-host-join-workspace = Join a workspace
session-host-join-workspace-description = From its directory
session-host-leave-workspace = Leave workspace
session-host-leave-workspace-description = Take it off this computer
session-host-workspace-settings = Workspace settings
session-host-workspace-settings-description = Its name and who is in it
session-host-lock = Lock
session-host-log-out = Log out
session-host-restart = Restart
session-host-shut-down = Shut down
session-host-report-problem = Report a problem
session-host-report-problem-description = Send what went wrong to Tilo
session-feedback-destination = Tilo feedback
session-left-workspace = You left { $name }
session-left-workspace-body = Its windows are gone from this computer. You can join it again from the workspace directory.
session-deleted-workspace = { $name } is deleted
session-deleted-workspace-body = It is gone from this computer and from your list of workspaces.
session-owner-deleted-workspace = { $name } was deleted by its owner
session-owner-deleted-workspace-body = It is gone from this computer, and nothing in it can be opened anymore.
session-joined-workspace = You joined { $name }
session-joined-workspace-body = It is open now, with everything its people keep in it.
session-join-failed-title = Couldn't join the workspace
session-awaiting-admission = This device isn't in { $name } yet
session-awaiting-admission-body = { $host } hosts { $name } and has to take this device in first. Its apps here wait until this device reaches { $host } once.
desktop-no-workspace = There is no workspace called { $name }
desktop-joined-workspace-unknown = That workspace shares a workspace this computer cannot join
desk-first-workspace = This is the first workspace
desk-last-workspace = This is the last workspace
desk-first-strip = This is the first strip
desk-last-strip = This is the last strip
desk-window-edge = Nothing changed. The window can't move any further that way.
desktop-no-strip = There is no strip called { $name } here
desktop-strip-not-here = That strip is not on this workspace any more
desktop-workspace-gone = That workspace is not here any more
desktop-no-workspace-to-leave = There is no workspace to leave
desktop-no-workspace-to-delete = There is no workspace to delete
desktop-only-workspace = This is the only workspace here
desktop-workspace-stays = This workspace stays: someone else is in it, or another of your devices holds it.
desktop-cannot-delete-workspace = This desktop cannot delete workspaces
desktop-window-not-here = That window is not on this desktop any more
desktop-nothing-to-move-to = There is nowhere to move that window
desktop-move-no-record = Moving a window keeps no record
desktop-actions-search = Search
session-input = Input devices
session-toggle-touchpad = Turn the touchpad on or off
session-param-touchpad-on = Whether the touchpad is on; leave out to switch it
session-next-input-source = Type in the next keyboard input source
session-screen = Screen
session-lock = Lock the screen
session-log-out = Log out
session-magnifier = Magnifier
session-magnifier-toggle = Turn the magnifier on or off
session-magnifier-zoom-in = Magnify more
session-magnifier-zoom-out = Magnify less
session-displays = Displays
session-displays-next = Next display arrangement
session-displays-set = Arrange the displays
session-clipboard = Clipboard
session-clipboard-paste-files = Paste files here
session-param-paste-destination = The folder the copied files are pasted into

session-window-share = Shared windows
session-window-share-share = Share a window with the workspace
session-window-share-param-window = The window, by the identifier its app may capture; leave out for the window the tab shows
session-window-share-param-tab = The tab the window is shown in on this device
session-window-share-param-app = The app the window belongs to; leave out for the app of the window the tab shows
session-window-share-param-workspace = The workspace to share it in
session-window-share-param-share-workspace = The workspace to share it in; leave out for the one the tab stands in
session-window-share-stop = Stop sharing the window
session-window-share-allow-input = Let the workspace control the window
session-window-share-param-on = Whether they may control it; leave out to switch it
session-window-share-watch = Show a window another member shares
session-window-share-unwatch = Stop showing it
session-window-share-workspace-left = This session is no longer in the workspace

## The on-screen readout for the hardware keys.
desktop-actions-osd = On-screen readout
desktop-action-show-osd = Show a readout
desktop-action-param-osd-kind = What it is about: volume, brightness, keyboard-backlight, microphone, airplane-mode, touchpad, play, pause, next-track, previous-track, stop, no-backlight, night-light, do-not-disturb, magnifier, captured-keys or pointer-released
desktop-action-param-osd-level = The level, from 0 to 1
desktop-action-param-osd-on = Whether it is on
osd-play = Play
osd-pause = Pause
osd-play-named = Playing { $song }
osd-pause-named = Paused { $song }
osd-next-track = Next track
osd-previous-track = Previous track
osd-stop = Stop
osd-muted = Muted
osd-percent = { $percent }%
osd-off = Off
osd-volume = Volume { $percent }%
osd-brightness = Brightness { $percent }%
osd-no-backlight = No adjustable display
osd-microphone-muted = Microphone muted
osd-microphone-on = Microphone on
osd-keyboard-backlight-off = Keyboard backlight off
osd-keyboard-backlight = Keyboard backlight { $percent }%
osd-airplane-mode-on = Airplane mode on
osd-airplane-mode-off = Airplane mode off
osd-touchpad-on = Touchpad on
osd-touchpad-off = Touchpad off
osd-volume-name = Volume
osd-brightness-name = Brightness
osd-keyboard-backlight-name = Keyboard backlight
osd-readout = Readout
osd-reading = { $device }, { $value }
osd-night-light-on = Night light on
osd-night-light-off = Night light off
osd-do-not-disturb-on = Do not disturb on
osd-do-not-disturb-off = Do not disturb off
osd-magnifier-on = Magnifier on
osd-magnifier-off = Magnifier off
osd-keys-captured = Keys captured
osd-input-source = Input source
osd-pointer-released = Pointer released
desktop-action-param-osd-device = The device it changed, as a person knows it
desktop-action-param-osd-device-glyph = The device's glyph: speaker, headphones, display or keyboard
desktop-action-param-osd-chord = The chord that asked for it
desktop-action-param-osd-display = The connector of the display it shows on
desktop-display-name = Display { $number }
# What a person can do to one window, from its own menu.
window-menu-reveal = Go to this window
window-menu-rename = Name this window…
window-tab-name = Window name
window-menu-full-screen = Full screen
window-menu-exit-full-screen = Exit full screen
window-menu-maximize = Fill the strip
window-menu-restore = Give the strip back
window-menu-float = Float this window
window-menu-snap-back = Snap back into the strip
window-menu-move-to-strip = Move to strip
window-menu-move-to-workspace = Move to workspace
window-menu-move-to-display = Move to display
window-menu-width = Width
window-menu-width-half = Half the strip
window-menu-width-narrow = Narrow
window-menu-width-normal = Normal
window-menu-width-wide = Wide
window-menu-width-equal = Every window the same
window-menu-wider = Wider
window-menu-narrower = Narrower
window-menu-widest = Widest
window-menu-narrowest = Narrowest
window-menu-new-tab = New tab of this app
window-menu-not-yours-to-open = Watching only
window-menu-no-app = No app here
window-menu-split-right = Split right
window-menu-split-one-tab = Nothing to split: this window has one tab
window-menu-new-group = Move to a new window
window-menu-close = Close
window-menu-close-window = Close this window
window-menu-stop-desktop-control = Stop controlling the desktop
# Sharing a window with its workspace shows its live picture in its tab on the workspace's other
# devices. The toolkit's "Share to channel…" posts the window in a channel instead: the two rows
# stand together, so each names where the window goes.
window-menu-share-with-workspace = Share with workspace
window-menu-stop-sharing-with-workspace = Stop sharing with workspace
window-menu-stop-sharing-watched = Stop sharing with workspace ({ $viewers } watching)
window-menu-someone-elses = { $name }'s window

# The desk's widgets: what each is called, and what its controls do.
desk-widget-music = Music
desk-widget-team = People here
desk-widget-calendar = Calendar
desk-widget-system-monitor = System monitor
desk-widget-pin = Always show this widget on this workspace
desk-widget-unpin = Stop always showing this widget
desk-widget-open = Open { $widget }
desk-widget-collapse = Fold { $widget } back
desk-widget-remove = Remove { $widget }
desk-widget-removed = { $widget } removed from the desktop
desk-widget-undo = Undo
desk-team-title = In this workspace
desk-team-more = +{ $count } more
desk-monitor-title = This machine
desk-monitor-cpu = Processor
desk-monitor-memory = Memory
desk-monitor-temperature = Temperature
desk-monitor-cpu-short = CPU
desk-monitor-memory-short = RAM
desk-widget-resize = Resize { $widget }

# A window whose app stopped answering its pings: what happened, and the two
# ways out of it.
hung-app-title = Not responding
hung-app-card = { $window } is not responding
hung-app-wait = Wait
hung-app-force-quit = Force quit
desk-wait-window = Keep waiting for { $window }
desk-force-quit-window = Force quit { $window }
activities-layer = Activities
activity-collapse = Put away
activity-close = Close

profile-agent-card = Open card
profile-agent-stop = Stop
profile-agent-undo-run = Undo run
profile-agent-take-over = Take over
profile-agent-revoke = Revoke access
profile-agent-now = Now
task-programs-resource = Task programs
task-programs-run = Run a program
task-programs-run-detail = Runs a tilolang program as a step of the task's run, as the agent working the task, and answers what it read or changed.
