## tilolang app window chrome. Everything a person reads inside an app comes from the app's own source.

tilolang-title = Apps
tilolang-home-heading = Apps
tilolang-home-empty = No apps are installed.
tilolang-open-failed = This app could not be opened.
tilolang-view-failed = This couldn't be shown
tilolang-seen-failed = What this window showed could not be marked as seen.
tilolang-back = All apps
tilolang-detail = Details
# The page a window showed of a resource the workspace no longer holds, wherever the removal came
# from: another surface, an agent or another device.
tilolang-gone = This was removed.
# The Ask bar's question, naming the action as its package does ("Allow “Search”?").
tilolang-ask-action = Allow “{ $action }”?
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
tilolang-change-kept = Your change is kept on this device until the workspace takes it.
# What a change reads as in history, worked out from the field it wrote and what it held before
# and after: a sentence goes on with it after who made it ("You moved this to Friday"). `$field`
# is the field's name in a sentence ("due", "priority").
tilolang-did-changed = changed { $field }
# A write of the field a resource is named by (a task's title), whatever its type.
tilolang-did-renamed = renamed
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
# A bound control's edit, where nothing of the field it wrote is left to say what changed.
tilolang-did-edited = edited it
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
# The people who changed one item since the person last looked, as a sentence names them ("Scout
# and Bo"); past three, the faces beside them count the rest.
tilolang-names-two = { $first } and { $second }
tilolang-names-three = { $first }, { $second } and { $third }
tilolang-names-more = { $first }, { $second } and { $count } others

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
tilolang-nothing-open = Nothing open
tilolang-empty = Nothing here yet
# What a list says when it lists none of its kind's items: "No notes yet".
tilolang-empty-kind = No { $kind } yet
# What a body says when it holds nothing yet, naming the first thing to make: "Start your first note".
tilolang-empty-start = Start your first { $noun }
# What an empty list says under its title, where the way to start one stands.
tilolang-empty-start-here = Start one here
# What a tree says where it stands when the window's search finds nothing in it.
tilolang-nothing-found = Nothing matches
# What a heading with no words is called in a map of a document's headings.
tilolang-untitled = Untitled
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
# A call held on a resource (a voice channel): the person's own controls, and what each person
# in it does.
tilolang-call-join = Join
tilolang-call-leave = Leave
tilolang-call-mute = Mute
tilolang-call-unmute = Unmute
tilolang-call-deafen = Deafen
tilolang-call-undeafen = Undeafen
tilolang-call-camera-on = Turn camera on
tilolang-call-camera-off = Turn camera off
tilolang-call-screen-on = Share screen
tilolang-call-screen-off = Stop sharing
tilolang-call-empty = No one is here
tilolang-call-empty-message = Join to talk; whoever joins next hears you.
tilolang-call-speaking = Talking
tilolang-call-muted = Muted
tilolang-call-deafened = Deafened
tilolang-call-on-camera = Camera
tilolang-call-sharing = Sharing screen
tilolang-call-from-discord = From Discord
tilolang-call-from = From { $service }

# Under a change of the person's own while no other device of the workspace has it yet, and once
# the workspace refused it.
tilolang-reach-pending = Not reached anyone yet
tilolang-reach-refused = The workspace refused this

app-activate-window = Show this window
app-activate-window-description = Bring this app's window to the front.
tilolang-form-cancel = Cancel
tilolang-none-value = None
tilolang-empty-value = Empty
tilolang-yes = Yes
tilolang-no = No
tilolang-history-of = History of { $title }
tilolang-history-changes = { $count ->
    [one] 1 change
   *[other] { $count } changes
}
tilolang-history-show = Show changes
tilolang-history-hide = Hide changes
# Read between what a value held and what it holds now, where an arrow shows it.
tilolang-change-to = to
# A step of the history holds its revert in its own menu, named by what the menu undoes.
tilolang-history-actions = Actions for this change
# Taking an older change back, as a new change that reverses it.
tilolang-history-revert = Revert this change

