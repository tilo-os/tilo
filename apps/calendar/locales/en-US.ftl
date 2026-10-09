# Calendar — user-facing strings.

## History: what each change did, by field and by verb

calendar-new-event = Added an event
calendar-new-task = Added a task
calendar-new-project = Added the project { $name }
calendar-delete-event = Deleted an event
calendar-reschedule = Moved an event
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
