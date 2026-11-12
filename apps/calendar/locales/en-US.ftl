# Calendar — user-facing strings.

## History: what each change did, by field and by verb

calendar-new-event = Added an event
calendar-new-task = Added a task
calendar-new-project = Added the project { $name }
calendar-delete-event = Deleted an event
calendar-reschedule = Moved an event
calendar-set-start = Moved when an event starts
calendar-move-to-calendar = Moved an event to another calendar
calendar-invite = Invited { $email }
calendar-delete-task = Deleted a task
calendar-delete-project = Deleted a project
calendar-import-items = Imported calendar items
calendar-adopt-remote-event = Took an event from its server
calendar-adopt-remote-task = Took a task from its server
calendar-drop-remote-item = Removed what a server no longer holds
calendar-server-applied = A calendar server took an event
calendar-server-removed = A calendar server deleted an event
calendar-pulled = Pulled a calendar
calendar-event-title = Renamed an event to { $new }
calendar-event-start = Moved the start of an event
calendar-event-end = Moved the end of an event
calendar-event-all-day = { $new ->
    [true] Made an event all day
   *[false] Gave an event its times
}
calendar-event-location = Changed where an event is
calendar-event-color = Changed the colour of an event
calendar-event-recurrence = Changed how an event repeats
calendar-event-recurrence-interval = Changed how often an event repeats
calendar-event-recurrence-until = Changed when an event stops repeating
calendar-event-recurrence-count = Changed how many times an event repeats
calendar-event-recurrence-exdates = Skipped or restored a date of an event
calendar-event-occurrence-overrides = Changed one date of an event
calendar-event-timezone = Changed the time zone of an event
calendar-event-organizer = Changed who organizes an event
calendar-event-attendees = Changed who is invited to an event
calendar-event-reminder-min = Changed the reminder of an event
calendar-event-project = Filed an event under another project
calendar-event-calendar-id = Moved an event to another calendar
calendar-attendee-name = Changed the name of an attendee
calendar-attendee-email = Changed the address of an attendee
calendar-attendee-status = Recorded an answer to an invitation
calendar-occurrence-override-cancelled = { $new ->
    [true] Cancelled one date of an event
   *[false] Restored one date of an event
}
calendar-occurrence-override-start = Moved one date of an event
calendar-occurrence-override-end = Moved the end of one date of an event
calendar-occurrence-override-title = Renamed one date of an event
calendar-occurrence-override-location = Changed where one date of an event is
calendar-task-title = Renamed a task to { $new }
calendar-task-done = { $new ->
    [true] Completed a task
   *[false] Reopened a task
}
calendar-task-due = Changed when a task is due
calendar-task-priority = Changed the priority of a task
calendar-task-project = Filed a task under another project
calendar-project-name = Renamed a project to { $new }
calendar-project-color = Changed the colour of a project
calendar-project-order = Moved a project
calendar-calendar-projects = Changed the projects
calendar-calendar-events = Changed the events
calendar-calendar-tasks = Changed the tasks
calendar-calendar-notes = Edited notes
calendar-calendar-uploads = Recorded how an upload to a calendar server went
calendar-calendar-deletes = Recorded how a delete on a calendar server went

## The Tasks screen

calendar-tasks-title = Tasks
calendar-view-month = Month
calendar-view-week = Week
calendar-view-day = Day
calendar-view-agenda = Agenda
calendar-tasks-new = New task
calendar-tasks-entry = Add a task
calendar-tasks-entry-name = New task title
calendar-tasks-empty-inbox = Inbox is empty
calendar-tasks-empty-today = Nothing due today
calendar-tasks-empty-upcoming = Nothing coming up
calendar-tasks-empty-completed = No completed tasks
calendar-tasks-empty-project = No tasks in { $list }
calendar-tasks-empty-completed-hint = Nothing has been finished yet.
calendar-tasks-empty-hint = Type a task in the field above and press Enter.
calendar-tasks-priority-none = No priority
calendar-tasks-priority-low = Low priority
calendar-tasks-priority-medium = Medium priority
calendar-tasks-priority-high = High priority
calendar-tasks-added = Added { $title }
calendar-change-refused = Calendar didn't keep that: { $reason }
calendar-tasks-list = Task list
calendar-tasks-add-to = Add a task to { $list }
calendar-tasks-row-due = { $title }, due { $due }
calendar-tasks-row-priority = { $title }, { $priority }
calendar-tasks-row-subtask = { $title }, subtask
calendar-tasks-empty-description = { $title }. { $message }
calendar-view-name = { $view } view
calendar-tasks-inbox = Inbox
calendar-tasks-today = Today
calendar-tasks-upcoming = Upcoming
calendar-tasks-completed = Completed

## The sidebar and what the app names to assistive technology

calendar-navigation = Calendar navigation
calendar-month = Month
calendar-projects = Projects
calendar-sync = Sync
calendar-open-task = Open task
calendar-close-list = Close list
calendar-list-empty = No tasks
calendar-project = Project

## What the window says of a sync, an import, an edit or a calendar
## changed, each in the severity the place that raised it gives it.

calendar-sync-running = Syncing calendars…
calendar-sync-running-short = Syncing…
calendar-sync-synced = Synced { $calendars } calendars, { $events } events.
calendar-sync-offline = Offline. Changes sync when you reconnect.
calendar-sync-conflict = 1 change also changed on the server; your version is safe and still queued.
calendar-sync-failed = Sync failed
calendar-sync-failed-hint = Sync failed. Check the account in Settings, then sync again.
calendar-sync-error = Sync failed: { $reason }. Check the account in Settings, then sync again.
calendar-sync-unreachable = Couldn't reach the calendar server
calendar-sync-unavailable = Sync unavailable: { $reason }
calendar-sync-no-accounts = No calendar accounts yet. Add one in Settings.
calendar-account-gone = That calendar account is no longer configured
calendar-invite-address-required = Enter a complete email address, such as name@example.com
calendar-invite-already = { $email } is already invited
calendar-invite-added = Added { $email }. Prepare the invitation in Mail to send it.
calendar-invite-prepared = Invitation prepared for Mail. Review it there before sending.
calendar-export-empty = Nothing to export yet
calendar-exported = Exported { $events } events and { $tasks } tasks
calendar-export-cancelled = Export cancelled
calendar-move-event-failed = Couldn't move the event: { $reason }
calendar-showing-all = Showing all calendars
calendar-showing-calendar = Showing { $calendar }
calendar-calendar-hidden = { $calendar } is hidden. Turn it on in the sidebar to show its events.
calendar-visible = Calendar is now visible
calendar-hidden = Calendar is now hidden

## A drag on an occurrence of a repeating event
calendar-repeat-change = Change repeating event
calendar-repeat-only-this = Only this event
calendar-repeat-all-future = All future events
calendar-repeat-failed = Couldn’t move the events: { $error }

## Attendees from the person's Contacts
calendar-contact-attendees = Invite from Contacts
calendar-contact-attendee = { $person } <{ $email }>
calendar-contact-add = Add as guest

## The one-line quick add: what it asks for, and what it will write

calendar-quick-title = Add an event
calendar-quick-placeholder = Lunch with Sam on Friday at 1pm
calendar-quick-field = What to add
calendar-quick-add = Add
calendar-quick-cancel = Cancel

## The keys this window answers to

calendar-help-title = Keyboard shortcuts
calendar-help-month = Month view
calendar-help-week = Week view
calendar-help-day = Day view
calendar-help-agenda = Agenda view
calendar-help-today = Go to today
calendar-help-previous = Previous period
calendar-help-next = Next period
calendar-help-previous-day = Previous day
calendar-help-next-day = Next day
calendar-help-previous-week = Previous week
calendar-help-next-week = Next week
calendar-help-new-event = New event, in full
calendar-help-quick-add = Add an event in one line
calendar-help-delete = Delete the selected item
calendar-help-close = Close the editor or card
calendar-help-shortcuts = This list of keys

## What the window is called and what its fields ask for

calendar-title = Calendar
calendar-a11y-syncing = Synchronizing calendars
calendar-a11y-editor = Event editor. Changes save automatically.
calendar-quick-example = Lunch with Sam friday 1pm
calendar-invite-placeholder = Invite by email
calendar-location-placeholder = Add location

## Window and accessibility copy
calendar-a11y-task-due = Task due date

## What a control reads out
calendar-a11y-today = Go to today
calendar-a11y-previous = Previous period
calendar-a11y-next = Next period
calendar-a11y-new-event = New event
calendar-a11y-new-task = New task
calendar-a11y-retry-sync = Retry calendar synchronisation
calendar-a11y-sync = Sync calendars
calendar-a11y-export = Export calendar
calendar-a11y-import = Import calendar
calendar-a11y-open-day = Open { $day }
calendar-a11y-discard-draft = Discard new event
calendar-a11y-delete-task = Delete task
calendar-a11y-all-day = All-day event
calendar-a11y-event-title = Edit event title
calendar-a11y-event-location = Edit event location
calendar-a11y-event-invite = Invite attendee by email
calendar-a11y-start-date = Choose event start date
calendar-a11y-end-date = Choose event end date
calendar-a11y-start-time = Choose event start time
calendar-a11y-end-time = Choose event end time
calendar-a11y-choose-task-due = Choose task due date
calendar-a11y-dismiss-picker = Dismiss task picker
calendar-a11y-due-previous-month = Previous due-date month
calendar-a11y-due-next-month = Next due-date month
calendar-a11y-clear-due = Clear task due date
calendar-a11y-set-due = Set task due date to { $day }
calendar-a11y-move-event-to = Move event to { $day }
