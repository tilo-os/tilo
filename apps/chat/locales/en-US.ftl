# Chat — user-facing strings.

chat-contacts-section = Contacts
chat-channels-title = Channels
chat-person-chat-private = Private conversation
# A message's author or a member, opening their profile card.
chat-person-card = { $name }, profile
chat-person-chat-placeholder = Write a private message
chat-person-chat-unavailable = Private conversation unavailable. Choose someone in this workspace.
chat-person-chat-empty = Write a message before sending.
chat-person-chat-empty-conversation = Messages here are private to you and this person.
chat-header-search = Search

# Marker beside a participant in the `@` picker that is an agent rather than a
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

# Emoji picker: the add-reaction button, the search field's name, the
# composer's emoji button and a group tab without a name of its own.
chat-emoji-add-reaction = Add reaction
chat-emoji-search = Search emoji
chat-emoji-add = Add emoji
chat-emoji-insert = Insert emoji
chat-emoji-group = Emoji group
# A picker cell; the name is the emoji's name in the desktop's language.
chat-emoji-choose = Choose { $name }

# Governed voice status, shared by native and browser hosts.
chat-call-issue-unavailable = Voice isn't available in this room yet
chat-call-issue-denied = You can't join calls in this room
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
chat-call-count = { $count ->
    [one] 1 in call
   *[other] { $count } in call
    }
chat-call-open = Open call conversation
chat-call-join = Join muted
chat-call-leave-action = Leave call
chat-call-mute-action = Mute microphone
chat-call-unmute-action = Request microphone
chat-call-status = Call status
chat-call-select-input = Select microphone
chat-call-input = Input
chat-call-input-hint = Input from call status

# The call window's connection line under its video.
chat-call-connected = Connected
chat-call-bridged = This call is bridged to { $services }
chat-call-bridged-short = Bridged to { $services }
chat-call-status-bridged = { $connection }. { $note }
chat-call-participant = { $name }: volume and mute for you
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
chat-agent-work-taken = Request accepted
chat-agent-work-waiting = Last reported: waiting for app state: { $app }
chat-agent-work-intent = Latest intent: { $action }
chat-agent-work-intent-about = Latest intent: { $action }: { $about }
chat-agent-work-done = Completed: { $action }
chat-agent-work-refused = Could not complete this request
chat-agent-work-model-rationale = Model rationale
chat-agent-work-rule-match = Rule match
chat-agent-work-model-choice = Chosen by the configured model
chat-agent-work-rule-choice = Matched against exported action vocabulary
chat-agent-work-rationale = { $route }: { $why }
chat-agent-work-step-requested = Requested: { $target }
chat-agent-work-step-applied = Applied: { $target }
chat-agent-work-step-no-change = No change: { $target }
chat-agent-work-step-refused = Refused: { $target }
chat-agent-work-step-refused-why = Refused: { $target }: { $why }
chat-agent-work-show-details = Show activity
chat-agent-work-hide-details = Hide activity
chat-agent-recovery = Review the partial answer before retrying or asking to continue. Check connectivity and provider settings; never repeat completed or unconfirmed operations.
chat-agent-provider-reasoning = Reasoning shared by the provider
chat-agent-show-reasoning = Show reasoning
chat-agent-hide-reasoning = Hide reasoning
chat-agent-working = Working
chat-agent-complete = Complete
chat-agent-truncated = Answer interrupted
chat-agent-refused = Request refused
chat-agent-cancelled = Stopped
chat-agent-failed = Could not finish
chat-model-ask = Ask assistant
chat-model-stop = Stop
chat-model-retry = Retry
chat-model-continue = Continue
chat-model-author = Assistant
chat-model-attribution = { $model } (model output, requested by { $person })
chat-bot-attribution = { $name } (bot: { $bot })
chat-via-attribution = { $name } (via { $service })
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
chat-context-following = Following live Chat context · Stop following in workspace controls
chat-context-stop = Stop sharing
chat-header-call-open = Open call

chat-identity-available = Available
chat-identity-in-voice = In voice

chat-resource-copy-link = Copy resource link
chat-resource-not-opened = Could not open
chat-agent-kind = Agent
chat-channel-empty-message = Say hello. Everyone in #{ $channel } sees it.
chat-agent-float = Float conversation
chat-agent-return = Return conversation
chat-send = Send
chat-conversation-read-only = You can read this conversation but not write in it.
chat-agent-author = { $name } (agent)
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
chat-share-pick = Share { $title }
chat-voice-unavailable = Voice calls are unavailable until a media service is connected.
chat-call-not-joined = Join the call first.
chat-attachment-import-unavailable = Attachment import is unavailable on this host.

# "You" belongs to the current reader; shared/legacy names identify their person.
chat-person-unnamed = Someone ({ $tag })
chat-person-relative-name = { $name } ({ $tag })

## History: what each change did, by verb and by field
chat-begin-chat = Started the chat
chat-introduce = Introduced themselves as { $name }
chat-name-agent = Named an agent { $name }
chat-create-category = Created the category { $name }
chat-create-channel = Created the channel #{ $name }
chat-create-channel-with-id = Created the channel #{ $name }
chat-delete-category = Deleted the category { $item }
chat-move-channel = { $up ->
    [true] Moved #{ $item } up
   *[false] Moved #{ $item } down
}
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
chat-post = Posted: { $text }
chat-post-file = Posted the file { $name }
chat-share-resource = Shared a link in #{ $item }
chat-deliver-message = Delivered a message from { $author }
chat-edit-message = Edited a message: { $text }
chat-delete-message = Deleted a message
chat-mark-read = Marked #{ $item } read
chat-record-work = Recorded what it is doing
chat-submit-feedback = Filed feedback in #{ $item }
chat-triage = Triaged feedback
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
chat-chat-people = { $op ->
    [insert] Added a person
    [remove] Removed a person
   *[other] Changed the people
}
chat-chat-feedback = { $op ->
    [insert] Added a feedback report
    [remove] Removed a feedback report
   *[other] Changed the feedback reports
}
chat-chat-agent-names = Changed the agents' names
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
chat-mention-alert = { $name } mentioned you in #{ $channel }

# What kind of thing a message row is, for assistive technology and the
# feedback picker ("Message in Chat").
chat-message-kind = Message

## A filed feedback report as its channel shows it
# One picked part of the screen: its place, what it was, and the app.
chat-feedback-pick = { $index }. { $what } in { $app }
# One picked part from an app whose name is not known: its place and what it
# was, never the app's id.
chat-feedback-pick-alone = { $index }. { $what }
# What a picked part was, when the app said what kind of thing it is.
chat-feedback-part-kind = { $kind }
# What a picked part was, by the app's own name for it and its role.
chat-feedback-part-named = The “{ $name }” { $role ->
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
# What a picked part was, known only by its role.
chat-feedback-part-role = { $role ->
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
chat-feedback-note = “{ $note }”
# One text a picked part showed, as reviewed.
chat-feedback-text = { $text }
# One of the reporter's last actions, by the label its app gives it;
# $outcome is done, refused or failed.
chat-feedback-action = { $outcome ->
    [refused] { $label } (refused)
    [failed] { $label } (failed)
   *[done] { $label }
}
# An action whose app gave it no label, by its id and resource kind.
chat-feedback-action-unnamed = { $action } on { $resource }
chat-feedback-recent = Just before: { $actions }
chat-feedback-slow = Slow: { $slow } of the last { $frames } frames over { $budget } ms, the slowest { $worst } ms
# A hidden span of a reported text, by what it hid.
chat-feedback-hidden = { $kind ->
    [name] ‹name›
    [email] ‹email›
    [phone] ‹phone›
    [address] ‹address›
    [account] ‹account number›
    [credential] ‹credential›
   *[personal] ‹personal›
}

## The conversation's own controls and labels
chat-surface = Chat
chat-surface-detail = Channels in the current Tilo workspace
chat-messages = Messages
chat-add-channel = Add channel
chat-create-menu = Create channel or category
chat-composer = Message composer
chat-send-message = Send message
chat-undo = Undo
chat-retry = Retry
chat-close = Close
# A message that did not reach the room; $reason says why.
chat-not-sent = Not sent: { $reason }
chat-message-deleted = Message deleted
chat-message-edited = Edited
# Beside a pinned message's time.
chat-message-pinned-marker = Pinned
chat-empty-title = No messages yet
chat-empty-title-read-only = Nothing has been said here yet
chat-jump-latest = Jump to latest
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
chat-call-expand = Expand
chat-call-expand-restore = Expand or restore call
chat-call-participants-show = Show call participants
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
chat-menu-report-message = Report message
chat-menu-report-sender = Report sender
chat-menu-ban = Ban from the room
chat-menu-volume = Volume
chat-attach-share = Share a file
