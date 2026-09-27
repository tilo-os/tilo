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
