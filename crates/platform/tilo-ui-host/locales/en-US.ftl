## tilolang host chrome. Everything a person reads inside an app comes from the app's own source.

tilolang-title = Apps
tilolang-home-heading = Apps
tilolang-home-empty = No apps are installed.
tilolang-open-failed = This app could not be opened.
tilolang-open-page = Open as page
tilolang-seen-failed = What this window showed could not be marked as seen.
tilolang-back = All apps
tilolang-detail = Details
tilolang-ask-title = Allow this change?
tilolang-ask-by = Requested by { $invoker }
tilolang-ask-action = { $action }
tilolang-ask-confirm = Allow
tilolang-ask-decline = Decline
tilolang-suggestion-accept = Accept
tilolang-suggestion-dismiss = Dismiss
# Who proposes what waits on an item, said in the line that carries what it would do
# ("Scout suggests: set the due date to Sat").
tilolang-suggestion-says = { $name } suggests:
tilolang-suggested-by = suggested by { $name }
tilolang-ask-always = Always allow
tilolang-ask-denied = That change is not allowed here.
tilolang-ask-needs-approval = That change needs approval first.
tilolang-embed-source = Block
# What a change reads as in history, worked out from the field it wrote and what it held before
# and after: a sentence goes on with it after who made it ("You moved this to Friday"). `$field`
# is the field's name in a sentence ("due", "priority").
tilolang-did-changed = changed { $field }
tilolang-did-created = created it
tilolang-did-removed = removed it
tilolang-did-moved = moved it to
tilolang-did-taken-out = moved it out of
tilolang-did-assigned = assigned it to
tilolang-did-unassigned = unassigned it
tilolang-did-set = set { $field } to
tilolang-did-cleared = cleared { $field }
tilolang-did-set-date = set the { $field } date to
tilolang-did-cleared-date = cleared the { $field } date
tilolang-did-set-time = set the { $field } time to
# A flag that is now set or cleared: the field it says what it is ("done"), never a value.
tilolang-did-marked = marked it { $field }
tilolang-did-unmarked = unmarked it { $field }
tilolang-did-cleared-time = cleared the { $field } time
# What a change made over many items at once says: the action's own words are written for one
# item ("moved this to"), so the sentence counts what it changed.
tilolang-did-many = changed { $count } { $kind }
# How one sentence goes on with the next field a change wrote ("… and set priority to High").
tilolang-did-and = and

# The catch-up a window's change mark opens: what changed since the person last looked, item by
# item, and the one move that marks it all seen.
tilolang-since-you-looked = Since you looked
tilolang-mark-all-seen = Mark all seen
# What the window says once the person marked a catch-up seen; its Undo takes the marks back.
tilolang-marked-seen = { $count ->
    [one] Marked 1 change seen
   *[other] Marked { $count } changes seen
}

tilolang-invocation-pending = Waiting
tilolang-invocation-done = Done
tilolang-invocation-suggested = Waiting for approval
tilolang-invocation-declined = Declined
tilolang-invocation-failed = Failed
tilolang-invocation-denied = Not allowed
tilolang-embed-error = This block could not run
tilolang-all = All { $kind }
# The actor of a history line the person looking made.
tilolang-you = You
# The person looking, where a sentence names them after a verb ("assigned it to you").
tilolang-you-lower = you
tilolang-empty = Nothing here yet
# What a list says when it lists none of its kind's items: "No notes yet".
tilolang-empty-kind = No { $kind } yet
# What a list's entry at its end says while it is empty.
tilolang-add = Add an item

# The title over the items of a list that are done with, folded away after the rest.
tilolang-done = Done
# The heading over the items of a list that name no group of their own.
tilolang-no-date = No date
# Said over the items of a list done with, where every one of them is: the list is not empty, it is finished.
tilolang-all-done = All done
# What one of an app's places says on another app's page when nothing stands in it.
tilolang-all-caught-up = All caught up
# A reference in rich text to something the reader cannot read, as its chip says it.
rich-text-reference-unreadable = Not shared with you
tilolang-none-selected = No { $kind } selected
# How many items a person chose, in the bar that acts on them.
tilolang-selected = { $count ->
    [one] 1 selected
   *[other] { $count } selected
}
tilolang-clear-selection = Clear selection
tilolang-sort = Sort
tilolang-undo = Undo
# Typed while the window was busy with a press that opened no field, and no field had the keyboard.
tilolang-typing-unplaced = Nothing took what you typed: “{ $typed }”
tilolang-did = { $action }: { $title }
tilolang-replying-to = Replying to
tilolang-thread-replies = { $count ->
    [one] 1 reply
   *[other] { $count } replies
}
tilolang-make = Make
tilolang-from = From
tilolang-linked = Made from this

app-activate-window = Show this window
app-activate-window-description = Ask the platform to reveal the existing window, keeping its document owner.
tilolang-form-cancel = Cancel
tilolang-none-value = None
tilolang-empty-value = Empty
tilolang-yes = Yes
tilolang-no = No
tilolang-history = History
tilolang-history-changes = { $count ->
    [one] 1 change
   *[other] { $count } changes
}
tilolang-history-show = Show changes
tilolang-history-hide = Hide changes
tilolang-history-undo-run = Undo run

# How a repeating item comes round again, and when it is reminded of. The count picks the plural
# ("every 2 weeks", "3 days after done"); the source words are the same, so a window with no
# catalogue still reads.
tilolang-repeat-every-days = { $count ->
    [one] Every day
   *[other] Every { $count } days
}
tilolang-repeat-every-weeks = { $count ->
    [one] Every week
   *[other] Every { $count } weeks
}
tilolang-repeat-every-months = { $count ->
    [one] Every month
   *[other] Every { $count } months
}
tilolang-repeat-every-years = { $count ->
    [one] Every year
   *[other] Every { $count } years
}
tilolang-repeat-weekdays = Every weekday
tilolang-repeat-weekday-after-done = The next weekday after done
tilolang-repeat-days-after-done = { $count ->
    [one] 1 day after done
   *[other] { $count } days after done
}
tilolang-repeat-weeks-after-done = { $count ->
    [one] 1 week after done
   *[other] { $count } weeks after done
}
tilolang-repeat-months-after-done = { $count ->
    [one] 1 month after done
   *[other] { $count } months after done
}
tilolang-repeat-years-after-done = { $count ->
    [one] 1 year after done
   *[other] { $count } years after done
}
tilolang-remind-when-due = When it is due
tilolang-remind-minutes-before = { $count ->
    [one] 1 minute before
   *[other] { $count } minutes before
}
tilolang-remind-minutes-after = { $count ->
    [one] 1 minute after
   *[other] { $count } minutes after
}
tilolang-remind-hours-before = { $count ->
    [one] 1 hour before
   *[other] { $count } hours before
}
tilolang-remind-hours-after = { $count ->
    [one] 1 hour after
   *[other] { $count } hours after
}
tilolang-remind-days-before = { $count ->
    [one] 1 day before
   *[other] { $count } days before
}
tilolang-remind-days-after = { $count ->
    [one] 1 day after
   *[other] { $count } days after
}
tilolang-remind-weeks-before = { $count ->
    [one] 1 week before
   *[other] { $count } weeks before
}
tilolang-remind-weeks-after = { $count ->
    [one] 1 week after
   *[other] { $count } weeks after
}
