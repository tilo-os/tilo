# Chat — user-facing strings.

chat-direct-messages-section = Direct messages
chat-channels-title = Channels
chat-person-chat-private = Private conversation
# A message's author or a member, opening their profile card.
chat-person-card = { $name }, profile
chat-person-chat-placeholder = Write a private message
chat-person-chat-empty = Write a message before sending.
chat-send-empty = Write a message, or attach something to send.
chat-send-needs-recipient = Choose who the message goes to.
chat-send-one-conversation = A conversation goes on its own: choose it, or the people to send to.

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
chat-call-only-you = Just you
chat-call-people-many = { $first }, { $second } and { $count } others
chat-call-open = Open call conversation
chat-call-join = Join muted
chat-call-leave-action = Leave call
chat-call-status = Call status

# The call window's connection line under its video.
chat-call-connected = Connected
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
chat-agent-work-waiting = Waiting for { $app }
chat-agent-work-did = { $count ->
    [one] Did one thing
   *[other] Did { $count } things
}
chat-agent-work-refused = Could not complete this request
chat-agent-work-step-requested = Requested: { $target }
chat-agent-work-step-applied = Applied: { $target }
chat-agent-work-step-no-change = No change: { $target }
chat-agent-work-step-refused = Refused: { $target }
chat-agent-work-step-refused-why = Refused: { $target }: { $why }
chat-agent-work-show-details = Show activity
chat-agent-work-hide-details = Hide activity
chat-agent-recovery = The answer stopped early. Ask again to continue.
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
chat-direct-unavailable = You can't open that conversation yet: this device hasn't loaded the workspace's chat.
chat-person-unknown = There's nobody in this workspace to message.
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
chat-voice-unavailable = Calls can't start in this workspace yet
chat-call-not-joined = Join the call first.

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
chat-call-open-window = Open the call in Chat
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
chat-menu-copy-text = Copy text
chat-message-more = More
chat-message-bar-delete-now = Delete message now
chat-message-bar-remove-now = Remove message now
chat-menu-report-message = Report message
chat-menu-report-sender = Report sender
chat-menu-ban = Ban from the workspace
chat-menu-volume = Volume
chat-attach-share = Share a file
chat-channel-composer-placeholder = Message #{ $channel }

## What the window shows
chat-writing = Writing…
chat-voice-channel-unnamed = The voice channel could not be named

## Actions

chat-effects-model-control = Ask about this message
chat-effects-show-agent-reasoning = Show reasoning
chat-effects-open-channel = Open conversation
chat-effects-open-category = Open category
chat-effects-toggle-category = Fold category
chat-effects-reveal-message = Reveal message
chat-effects-reveal-message-range = Reveal messages
chat-effects-open-person = Open person
chat-effects-open-agent = Open agent conversation
chat-effects-open-direct-conversation = Open direct conversation
chat-effects-send-direct-message = Send direct message
chat-effects-send = Send message
chat-effects-export-content = Export conversation
chat-effects-attach-file = Attach file
chat-effects-share-live-context = Share live context
chat-effects-stop-sharing-live-context = Stop sharing live context
chat-effects-live-context-status = Live context status
chat-effects-open-slack-channel = Open Slack channel in browser
chat-effects-open-discord-channel = Open Discord channel in browser
chat-effects-join-call = Join call
chat-effects-leave-call = Leave call
chat-effects-mute = Mute
chat-effects-unmute = Unmute
chat-effects-deafen = Deafen
chat-effects-undeafen = Undeafen
chat-effects-camera-on = Turn camera on
chat-effects-camera-off = Turn camera off
chat-effects-share-screen = Share screen
chat-effects-stop-sharing-screen = Stop sharing screen
chat-effects-call-status = Call status
chat-effects-start-call = Start a call
chat-effects-start-video-call = Start a video call
chat-effects-select-input = Select microphone

## What the app is called in a window and in the launcher
chat-app-title = Chat
## What the records are called, and the actions on them
chat-drafts = Chat drafts
chat-workspace = Chat
chat-drafts-start-draft = Start a draft
chat-messages-post = Post a message
chat-workspace-begin-chat = Start the chat
chat-messages-post-file = Post a file
chat-messages-share-resource = Share live
chat-messages-share-window = Share a window live
chat-messages-stop-sharing = Stop sharing
chat-messages-set-share-access = Change what a shared window lets people do
chat-messages-share-open = Mark a shared window open or closed
chat-messages-deliver-message = Deliver message
chat-messages-edit-message = Edit message
chat-messages-delete-message = Delete message
chat-messages-remove-message = Remove message
chat-messages-pin = Pin message
chat-messages-retry = Ask again for an answer
chat-messages-record-work = Record agent work
chat-messages-carry-counts = Carry the counts of the parts before
chat-messages-stream-output = Record an answer as it arrives
chat-workspace-introduce = Introduce yourself
chat-workspace-name-agent = Name an agent
chat-workspace-create-category = New category
chat-workspace-create-channel = New channel
chat-workspace-create-channel-with-id = New channel with an id
chat-workspace-delete-category = Delete category
chat-workspace-move-channel = Move a channel
chat-workspace-move-category = Move a category
chat-workspace-arrange-channels = Arrange channels
chat-workspace-set-channel-category = File a channel
chat-workspace-rename-channel = Rename channel
chat-workspace-set-channel-topic = Set channel topic
chat-workspace-archive-channel = Archive channel
chat-workspace-rename-category = Rename category
chat-workspace-delete-channel = Delete channel
chat-workspace-report-message = Report message
chat-workspace-report-person = Report person
chat-workspace-decide-reports = Decide on a report
chat-workspace-overturn-decision = Overturn a decision
chat-workspace-mark-read = Mark as read
chat-workspace-submit-feedback = Submit feedback
chat-workspace-triage = Triage feedback
