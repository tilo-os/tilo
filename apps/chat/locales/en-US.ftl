# Chat — user-facing strings.

chat-direct-messages-section = Direct messages
chat-channels-title = Channels
chat-person-chat-private = Private conversation
# A message's author or a member, opening their profile card.
chat-person-card = { $name }, profile
chat-person-chat-placeholder = Write a private message
chat-assistant-composer-placeholder = Message your assistant
chat-person-chat-empty = Write a message before sending.
chat-send-empty = Write a message, or attach something to send.
chat-send-needs-recipient = Choose who the message goes to.
chat-send-one-conversation = A conversation goes on its own: choose it, or the people to send to.

# Marker beside a member in the `@` picker that is an agent rather than a
# person, so somebody can tell at a glance that they are addressing software.
chat-mention-agent-tag = agent

# Control buttons along the bottom of the expanded call overlay.
chat-call-mute = Mute
chat-call-unmute = Unmute
# The muted microphone of a call on a device with no microphone: why it stays muted.
chat-call-no-microphone = No microphone
chat-call-deafen = Deafen
chat-call-undeafen = Undeafen
chat-call-audio-only = Audio only
chat-call-collapse = Collapse
chat-call-leave = Leave
chat-call-camera-on = Camera
chat-call-camera-off = Stop camera
chat-call-share-screen = Share screen
chat-call-stop-sharing = Stop sharing

# Emoji picker: the add-reaction button, the search field's name and a group
# tab without a name of its own.
chat-emoji-add-reaction = Add reaction
chat-emoji-search = Search emoji
chat-emoji-add = Add emoji
chat-emoji-group = Emoji group
# A picker cell; the name is the emoji's name in the desktop's language.
chat-emoji-choose = Choose { $name }

# Governed voice status, shared by native and browser hosts.
chat-call-issue-unavailable = Call audio can't start on this device
chat-call-issue-denied = You can't join calls in this workspace
chat-call-issue-permission-denied = Microphone blocked. Allow it in settings, then unmute.
chat-call-issue-source-lost = Microphone disconnected
chat-call-issue-capture-failed = Microphone could not start
chat-call-issue-transport-failed = Call disconnected
chat-call-issue-session-ended = Call ended
chat-call-issue-stale = Call session expired
chat-call-issue-camera-denied = Camera blocked. Allow it in settings, then turn it on.
chat-call-issue-camera-lost = Camera stopped
chat-call-issue-screen-denied = Screen sharing was not allowed
chat-call-issue-screen-lost = Screen sharing stopped
chat-call-issue-unsupported = This browser can't join calls. Use a current Chrome, Edge, Firefox or Safari.

chat-call-title = Call: { $channel }
# Who a call is with, as the bar's pill and the card's hero name them: the
# people in it, never the channel's slug.
chat-call-people-two = { $first } and { $second }
# A call nobody else has joined yet, as the card's hero names it.
# What a person without a name reads their own lines under.
chat-person-you = You
chat-call-only-you = Just you
chat-call-people-many = { $first }, { $second } and { $count } others
chat-call-open = Open call conversation
chat-call-join = Join muted
chat-call-leave-action = Leave call
chat-call-status = Call status

# The call window's connection line under its video.
chat-call-connected = Connected
chat-call-member = { $name }: volume and mute for you
chat-call-silence = Mute for me
chat-call-volume = Volume { $percent }%
chat-call-reconnecting = Reconnecting
chat-call-joining = Joining
chat-call-voice-unavailable = Voice unavailable

# An attachment the reader has no access to: the card says why it cannot open.
chat-attachment-no-access = You don't have access to this file
chat-attachment-close = Close
chat-attachment-loading = Loading the picture…
chat-attachment-save-title = Save the attachment
chat-attachment-save-accept = Save
chat-attachment-save-failed = Couldn't save the file
chat-attachment-not-on-device = This file isn't on this device
chat-attachment-saved = Saved a copy
chat-call-unavailable = Unavailable

# Effects on the Chat root that start a call in the active workspace (Home's
# "Plan a meeting", the palette, agents). Video is published but unavailable.
chat-start-call = Start a call
chat-start-video-call = Start a video call

# Durable evidence attached to an agent's reply; availability comes from presence.
chat-agent-work-waiting = Waiting for { $app }
chat-agent-work-did = { $count ->
    [one] Did one thing
   *[other] Did { $count } things
}
# The one line of an agent at work: the step it is at ($what) and how many it has taken.
chat-agent-work-at-step = { $what } ({ $count ->
    [one] 1 step
   *[other] { $count } steps
})
# A step ($step) and what it is about ($about), both in the system's own words.
chat-agent-work-step-about = { $step }: { $about }
chat-agent-work-refused = Could not complete this request
chat-agent-work-step-requested = Requested: { $target }
chat-agent-work-step-applied = Applied: { $target }
chat-agent-work-step-no-change = No change: { $target }
chat-agent-work-step-refused = Refused: { $target }
chat-agent-work-step-refused-why = Refused: { $target }: { $why }
chat-agent-work-show-details = Show activity
chat-agent-work-hide-details = Hide activity
chat-agent-provider-reasoning = Reasoning shared by the provider
chat-agent-show-reasoning = Show reasoning
chat-agent-hide-reasoning = Hide reasoning
chat-agent-working = Working
chat-agent-complete = Complete
chat-agent-truncated = Answer interrupted
chat-agent-refused = Request refused
chat-agent-cancelled = Stopped
chat-agent-stop = Stop the answer
chat-agent-stopped-by-you = Stopped by you
chat-agent-stopped-by = Stopped by { $person }
# A stop of an agent's work that Chat did not take; $reason says why.
chat-agent-not-stopped = Not stopped: { $reason }
chat-agent-failed = Could not finish

# What answered a reply: the model ($model), its provider ($provider) and what the turn cost
# ($cost), with the tokens it used ($tokens) where its provider counted them.
chat-agent-answered = { $model }, { $provider }, { $cost }
# The model a conversation with an agent answers on, before the server says which one, and the choice
# that follows the workspace's default again.
chat-assistant-setup = Set up personal assistant
chat-open-assistant = Open Assistant
chat-review-proposals = Review proposals
chat-assistant-grant-missing = Your assistant needs permission to reply in this workspace.
chat-assistant-grant-choices = Choose what your assistant can use here for six days. Assistant work includes Chat, Tasks, Assistant and Shortcuts.
chat-assistant-grant-request = Request Chat only
chat-assistant-grant-work = Request assistant work
chat-assistant-grant-pending = Open this workspace in the browser where you created or restored your identity to approve the request.
chat-assistant-grant-code = Compare code { $code } in your approving browser.
chat-assistant-grant-withdraw = Withdraw request
chat-assistant-grant-refresh = Check approval
chat-assistant-grant-denied = Your request was declined. You can send a new request.
chat-assistant-grant-expired = Your assistant’s permission expired. Request access to continue.
chat-assistant-grant-revoked = Your assistant’s access was revoked. You can request access again.
chat-assistant-grant-connecting = Access is approved. Waiting for the workspace host to connect your assistant.
chat-assistant-grant-without-chat = The current permission does not include Chat. Revoke it before requesting different access.
chat-assistant-grant-active = Your assistant has access in this workspace.
chat-assistant-grant-manage = Manage assistant access
chat-assistant-grant-revoke = Revoke access
chat-assistant-grant-scope = Allows access to { $apps }.
chat-assistant-grant-until = Expires on { $date }
chat-assistant-grant-all-apps = all apps
chat-assistant-app-tasks = Tasks
chat-assistant-app-assistant = Assistant
chat-assistant-app-shortcuts = Shortcuts
chat-assistant-grant-cancelled = The request was cancelled.
chat-agent-model-settings = Connect a model provider
chat-assistant-needs-person = Sign in as yourself to set up your assistant.
chat-agent-model = Model
chat-agent-model-default = Follow the default model
# What this conversation's answers have cost so far ($cost), the model switcher's second line.
chat-agent-model-cost = Answers so far: { $cost }
chat-agent-answered-tokens = { $model }, { $provider }, { $cost }, { $tokens ->
    [one] 1 token
   *[other] { $tokens } tokens
}
chat-model-ask = Ask assistant
chat-model-stop = Stop
chat-model-retry = Retry
chat-model-continue = Continue
chat-model-author = Assistant
chat-model-attribution = { $model }, requested by { $person }
chat-bot-attribution = { $name } (bot: { $bot })
chat-via-attribution = { $name }, from { $service }
chat-bridge-name = { $service } bridge
chat-bot-name = Bot
chat-thread-start = Thread start
chat-thread-end = Latest reply
chat-thread-title = Thread
chat-thread-close = Close thread
chat-thread-replies = { $count ->
    [one] 1 reply
   *[other] { $count } replies
    }

chat-context-sharing = Sharing live Chat context
chat-context-following = Following live Chat context
chat-context-stop = Stop sharing
chat-header-call-open = Open call

chat-identity-available = Available
chat-identity-in-voice = In voice
chat-member-in-channel = In { $channel }

chat-resource-copy-link = Copy link
chat-resource-not-opened = Could not open
chat-agent-kind = Agent
chat-channel-empty-message = Say hello. Everyone in #{ $channel } sees it.
chat-send = Send
chat-voice-channel = Voice channel
chat-quote-caption = In reply to
chat-quote-earlier = A message above
chat-conversation-read-only = You can read this conversation but not write in it.
chat-direct-unavailable = You can't reach that conversation yet: this device is still loading the workspace.
chat-person-unknown = There's nobody in this workspace to message.
chat-resource-link-copied = Link copied
chat-resource-link-unavailable = Link unavailable

## A window shared live into a channel
chat-share-window = Share a window
chat-share-window-detail = A live view others can open
chat-share-window-to = Share a window in { $place }
chat-share-window-button = Share
chat-share-your-windows = Your windows
chat-share-no-windows = Open the window you want to share, then come back here.
chat-share-view-only = View only
chat-share-already-edit = You can already edit this
chat-share-can-edit = Can edit
chat-share-live = Live
chat-share-ended = No longer shared
chat-share-stop = Stop sharing
chat-share-loading = Loading the live view…
chat-share-open-to-see = Open it to see it live
chat-share-open = Open { $title }, shared by { $name }
chat-share-work-here = Work on { $title } here, shared by { $name }
chat-share-read-here = Read { $title } here, shared by { $name }
chat-share-open-window = Open { $title } in a window
chat-share-make-smaller = Make { $title } smaller
chat-share-pick = Share { $title }
chat-voice-unavailable = Calls can't start in this workspace yet
chat-call-not-joined = Join the call first.

# "You" belongs to the current reader; shared/legacy names identify their person.
chat-person-relative-name = { $name } ({ $tag })

## History: what each change did, by verb and by field
chat-begin-chat = Started the chat
chat-create-category = Created the category { $name }
chat-create-channel = Created the channel #{ $name }
chat-create-channel-with-id = Created the channel #{ $name }
chat-delete-category = Deleted the category { $item }
chat-move-channel = { $up ->
    [true] Moved #{ $item } up
   *[false] Moved #{ $item } down
}
chat-move-category = { $up ->
    [true] Moved the category { $item } up
   *[false] Moved the category { $item } down
}
chat-arrange-channels = Rearranged the channels
chat-delete-channel = Deleted the channel #{ $item }
chat-rename-channel = Renamed #{ $item } to #{ $name }
chat-rename-category = Renamed the category { $item } to { $name }
chat-set-channel-topic = Set the topic of #{ $item } to { $topic }
chat-set-channel-category = Moved #{ $item } to another category
chat-archive-channel = { $archived ->
    [true] Archived #{ $item }
   *[false] Restored #{ $item }
}
chat-start-draft = Started a draft
## History: a post is quoted by the one line its item records, never its whole body.
chat-post = Posted “{ $item }”
chat-post-file = Posted the file { $name }
chat-post-view = Posted the view “{ $item }”
chat-share-resource = Shared a link in #{ $item }
chat-deliver-message = Delivered a message from { $author }
chat-edit-message = Edited a message: { $text }
chat-delete-message = Deleted a message
chat-mark-read = Marked #{ $item } read
chat-keep-summary = Kept a summary of the conversation
chat-record-work = Recorded what it is doing
chat-submit-feedback = Filed feedback in #{ $item }
chat-triage = Triaged feedback
chat-write-skill = Added the skill { $name }
chat-correct-skill = Corrected the skill { $item }
chat-keep-example = Kept an example for { $item }
chat-drop-example = Removed an example from { $item }
chat-forget-skill = Removed the skill { $item }
chat-chat-categories = { $op ->
    [insert] Added a category
    [remove] Removed a category
   *[other] Reordered the categories
}
chat-chat-channels = { $op ->
    [insert] Added a channel
    [remove] Removed a channel
   *[other] Reordered the channels
}
chat-chat-feedback = { $op ->
    [insert] Added a feedback report
    [remove] Removed a feedback report
   *[other] Changed the feedback reports
}
chat-chat-read = Changed the read marks
chat-chat-drafts-drafts = { $op ->
    [splice] Typed in a draft
    [mark] Formatted a draft
    [replace] Cleared a draft
   *[other] Edited a draft
}
chat-message-pinned = { $new ->
    [true] Pinned a message
   *[false] Unpinned a message
}
chat-person-name = Changed their name to { $new }
chat-person-initials = Changed their initials to { $new }
chat-person-color = Changed their colour
chat-person-status = Set their status to { $new ->
    [online] online
    [idle] away
    [dnd] do not disturb
    [offline] offline
   *[other] { $new }
}

## Making and changing channels
chat-sheet-new-channel = Create channel
chat-sheet-new-category = Create category
chat-sheet-edit-channel = Edit channel
chat-sheet-create-channel = Create channel
chat-sheet-create-category = Create category
chat-sheet-save = Save
chat-sheet-cancel = Cancel
chat-sheet-kind = Channel type
chat-sheet-text = Text
chat-sheet-text-about = Messages, pictures, files and threads
chat-sheet-voice = Voice
chat-sheet-voice-about = Talk, share your screen and see each other
chat-sheet-channel-name = Name
chat-sheet-category-name = Category name
chat-sheet-text-placeholder = new-channel
chat-sheet-voice-placeholder = Lounge
chat-sheet-category-placeholder = Projects
chat-sheet-category = Category
chat-sheet-no-category = None
chat-sheet-topic = Topic
chat-sheet-topic-placeholder = What this channel is for (optional)

# What kind of thing a message row is, for assistive technology and the
# feedback picker ("Message in Chat").
chat-message-kind = Message

## A filed feedback report's recording
chat-feedback-replay = Replay the recording
chat-feedback-replay-failed = The recording did not open
chat-feedback-replay-description = Open the recording in { $app } and step through it

## The conversation's own controls and labels
chat-surface = Chat
chat-surface-detail = Channels in this workspace
chat-messages = Messages
chat-earlier-arriving = Earlier messages are on their way
chat-add-channel = Add channel
chat-create-menu = Create channel or category
chat-composer = Message composer
chat-send-message = Send message
chat-undo = Undo
chat-retry = Retry
chat-failure-details = Details
chat-close = Close
# A message that did not reach the workspace; $reason says why.
chat-not-sent = Not sent: { $reason }
chat-message-deleted = Message deleted
chat-message-edited = Edited
# Beside a pinned message's time.
chat-message-pinned-marker = Pinned
# Where a message's time stands while the conversation it goes to is still
# arriving on this device; it is written once it arrived.
chat-message-sending = Sending…
chat-empty-title = No messages yet
chat-empty-title-read-only = Nothing has been said here yet
chat-assistant-look-round = Ask your assistant to check this workspace now. You can also ask it to show your scheduled checks, turn them on or off, change their timing, or lower their daily spending limit.
chat-jump-latest-message = Jump to latest message
chat-right-flap = Show people or thread
chat-typing-one = { $name } is typing…
chat-typing-two = { $first } and { $second } are typing…
chat-typing-several = Several people are typing…

## Pinned messages
chat-pins = Pinned messages
chat-pinned-message = Pinned message
chat-pins-empty = Nothing pinned yet
chat-pins-empty-detail = Pin a message from its menu to keep it here for everyone.

## Threads
chat-thread-open = Open message thread
chat-thread-composer = Thread reply composer
chat-thread-reply-placeholder = Reply in thread
chat-thread-last-reply = Last reply { $time }

## Editing a message
chat-edit-field = Edit message
chat-edit-save = Save message edit
chat-edit-cancel = Cancel message edit
chat-edit-save-button = Save
chat-edit-cancel-button = Cancel
chat-edit-history-show = Show edit history
chat-edit-history = Edit history
chat-edit-history-current = Current, { $time }
chat-edit-history-earlier = Earlier, { $time }

## Sharing a file
chat-attach = Share a file
chat-attach-device = Upload from this device
chat-attach-device-detail = Files, pictures and videos up to 32 MiB
chat-attach-workspace = From the workspace
chat-attach-more = { $count } more in the workspace
chat-attach-pick = Attach { $name }
chat-attach-picker-title = Share files
chat-attach-picker-accept = Share

## Calls
chat-call-start-or-open = Start or open call
chat-call-active = Active call
# A call whose channel has no name to show.
chat-call-untitled = Call
chat-call-open-window = Open the call in Chat
chat-call-expand = Expand
chat-call-expand-restore = Expand or restore call
chat-call-members-show = Show call members
chat-call-camera-off-caption = Camera off
chat-call-camera-starting = Your camera is starting
chat-call-no-video = No video
chat-call-audio-stream = Audio stream
chat-call-screen-mine = You're sharing your screen
chat-call-screen-mine-detail = Everyone in the call sees your screen
chat-call-screen-mine-chip = Your screen
chat-call-screen-theirs = { $name } is sharing their screen
chat-call-screen-theirs-chip = { $name }'s screen
chat-call-screen-waiting = Waiting for the first frame

## A channel's, category's or message's menu
chat-menu-rename = Rename
chat-menu-rename-channel = Rename channel
chat-menu-rename-category = Rename category
chat-menu-edit-channel = Edit channel
chat-menu-delete = Delete
chat-menu-delete-channel = Delete channel
chat-menu-delete-category = Delete category
chat-menu-move-up = Move up
chat-menu-move-down = Move down
chat-menu-new-channel = New channel
chat-menu-new-voice-channel = New voice channel
chat-menu-new-category = New category
chat-menu-collapse-category = Collapse category
chat-menu-expand-category = Expand category
chat-menu-archive-channel = Archive channel
chat-menu-reply-in-thread = Reply in thread
chat-menu-edit-message = Edit message
chat-menu-pin-message = Pin message
chat-menu-unpin-message = Unpin message
chat-menu-delete-message = Delete message
chat-menu-copy-text = Copy text
chat-message-more = More
chat-message-bar-delete-now = Delete message now
chat-message-bar-remove-now = Remove message now
chat-menu-report-message = Report message
chat-moderation-record-unavailable = Moderation is still loading. Try again in a moment.
chat-menu-report-sender = Report sender
chat-menu-ban = Ban from the workspace
chat-menu-volume = Volume
chat-attach-share = Share a file
chat-channel-composer-placeholder = Message #{ $channel }

## What the window shows
chat-writing = Writing…
chat-voice-channel-unnamed = The voice channel could not be named

## Actions


## What the app is called in a window and in the launcher
chat-app-title = Chat
## What the resources are called, and the actions on them
chat-messages-post = Post a message
chat-messages-post-file = Post a file
chat-messages-share-resource = Share live
chat-messages-share-window = Share a window live
chat-messages-stop-sharing = Stop sharing
chat-messages-set-share-access = Change what a shared window lets people do
chat-messages-share-open = Mark a shared window open or closed
chat-messages-deliver-message = Deliver message
chat-messages-import-message = Import message
chat-messages-edit-message = Edit message
chat-messages-delete-message = Delete message
chat-messages-remove-message = Remove message
chat-messages-pin = Pin message
chat-messages-retry = Ask again for an answer
chat-messages-ask = Ask your person
chat-messages-answer-question = Answer a question
chat-messages-withdraw-question = Withdraw a question
chat-messages-settle-question = Say what came of an answer
chat-messages-ask-agent = Ask an agent
chat-messages-stop-agent = Stop an agent’s work
chat-messages-post-view = Post a view
chat-messages-answer-with-view = Answer with a view
chat-messages-rewrite-view = Write a view again
# What started a turn nobody typed, named where an author’s name would stand.
chat-cause-reminder = Reminder: { $label }
chat-cause-event = Event: { $label }
chat-cause-news = News: { $label }
chat-view-attached = A view is attached
chat-view-attached-of = A view of { $apps } is attached
chat-view-alone = View
chat-view-waiting = This message is not on this device yet
chat-view-gone = This message carries no view
chat-view-arriving = Loading the view
chat-view-unavailable = A view cannot be shown here
chat-view-unbound = This view says you wrote it. Tilo cannot yet be sure who wrote a message, so it is not shown here
chat-view-refused = You may not read { $apps } here, so the view shows nothing of it
chat-view-unread = { $apps } could not be read here
chat-view-asks = { $who } wrote this view. It asks to do this in { $app }
chat-view-asks-given = { $name }: { $value }
chat-view-allow = Allow once
chat-view-dismiss = Dismiss
# The question a message asks one person: what an action that asks first would
# do, its answers, and how it stands once it no longer waits.
chat-question-on = On “{ $resource }”
chat-question-field = { $label }: { $value }
chat-question-final = It can’t be undone.
chat-question-allow = Allow once
chat-question-always = Always allow for this event
chat-question-deny = Don’t allow
chat-question-allowed = allowed once
chat-question-always-allowed = always allowed for this event
chat-question-with-item = From your vault: { $item }
chat-question-always-item = Always allow with this item here
chat-question-always-item-allowed = always allowed with this item here
chat-question-denied = not allowed
chat-question-you-answered = You answered: { $answer }
chat-question-answered = { $name } answered: { $answer }
chat-question-waiting = Waiting for { $name }
chat-question-waiting-someone = Waiting for an answer
chat-question-someone-answered = Answered: { $answer }
chat-question-started = Working on it
chat-question-done = Done
chat-question-dropped = Nothing was done
chat-question-failed = It could not be done
chat-question-withdrawn = No longer asked
chat-question-withdrawn-why = No longer asked: { $why }
chat-messages-record-work = Record agent work
chat-messages-carry-counts = Carry the counts of the parts before
chat-messages-stream-output = Record an answer as it arrives
# A message moved from another chat: its author there, and the tool.
chat-origin-author = { $name } in { $source }
# A question that asks for a secret, under the message that asks.
chat-secret-title-login = Password for { $place }
chat-secret-title-key = Key for { $place }
chat-secret-title-code-generator = Setup key for sign-in codes at { $place }
chat-secret-title-ssh-key = SSH key for { $place }
chat-secret-title-value = Value for { $place }
chat-secret-unreadable = This request for a secret does not say what it is for
chat-secret-waiting-mine = It goes to your vault. Whoever asked can use it there and cannot read it.
chat-secret-answer-in-window = Open this conversation in Chat to answer
chat-secret-filled = Kept in the vault
chat-secret-declined = Not given
chat-secret-field-login = Password
chat-secret-field-key = Key
chat-secret-field-code-generator = Setup key
chat-secret-field-ssh-key = Private key
chat-secret-field-value = Value
chat-secret-save = Save to vault
chat-secret-decline = Decline
chat-secret-saving = Adding it to your vault…
chat-secret-not-kept = The vault did not keep it. Try again.
chat-secret-answer-saved = It is in my vault now.
chat-secret-answer-declined = I would rather not give that.

