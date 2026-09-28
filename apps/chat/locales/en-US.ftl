# Chat — user-facing strings.

# Marker beside a participant in the `@` picker that is an agent rather than a
# person, so somebody can tell at a glance that they are addressing software.
chat-mention-agent-tag = agent

# Status line under the user's own name in the sidebar's identity bar.
chat-identity-available = Available
chat-identity-in-voice = In voice

# Chip under the workspace title that adds a channel category.
chat-sidebar-add-category = ＋ Category

# Control buttons along the bottom of the expanded call overlay.
chat-call-mute = Mute
chat-call-unmute = Unmute
chat-call-deafen = Deafen
chat-call-undeafen = Undeafen
chat-call-audio-only = Audio only
chat-call-collapse = Collapse
chat-call-leave = Leave

# Feedback channels: top-level messages are reports with votes and a status.
chat-feedback-toggle = Feedback channel on / off
chat-feedback-header = Reports
chat-feedback-sort-votes = Top
chat-feedback-sort-new = New
chat-feedback-filter-active = Open
chat-feedback-filter-closed = Closed
chat-feedback-filter-all = All
chat-feedback-empty = No reports match this filter.

chat-report-vote = Vote
chat-report-votes = { $count ->
    [one] { $count } vote
   *[other] { $count } votes
}
chat-report-status-open = Open
chat-report-status-planned = Planned
chat-report-status-in-progress = In progress
chat-report-status-fixed = Fixed in generation { $generation }
chat-report-status-wont-fix = Won't fix
chat-report-status-duplicate = Duplicate
chat-report-set-status = Set status
chat-report-set-fixed = Fixed in generation…
chat-report-set-duplicate = Duplicate of…
chat-report-pick-original = Choose the report this one duplicates
chat-report-original = This is the original
chat-report-cancel = Cancel
chat-report-fixed-generation-hint = Generation number, then Enter
chat-report-your-build-fixed = Your build contains the fix

# The chip on a message that was written about something.
chat-reference-open = Open { $label }

# A recorded session attached to a message.
chat-session-title = Recording
chat-session-layer-logic = Logic
chat-session-layer-below = Below the toolkit
chat-session-steps = { $count ->
    [one] { $count } step
   *[other] { $count } steps
}
chat-session-redactions = { $count ->
    [0] nothing redacted
    [one] { $count } redaction
   *[other] { $count } redactions
}
chat-session-replay = Replay
chat-session-withdraw = Withdraw
chat-session-withdrawn = Withdrawn from this room. Anyone who already opened it keeps their copy.
chat-record-title = Report

# What could not be done, in the reader's words.
chat-notice-replay-unavailable = Replay isn't available: { $reason }
chat-notice-share-failed = The recording wasn't shared into this room: { $reason }
chat-notice-unshare-failed = This room still references the recording: { $reason }
chat-notice-open-failed = Couldn't open it: { $reason }
chat-notice-message-failed = Couldn't open the conversation: { $reason }
chat-notice-invite-failed = Couldn't invite them: { $reason }
chat-notice-failed = That didn't work: { $reason }

# A participant's card.
chat-person-message = Message
chat-person-invite = Invite to workspace

# How the app names the local participant and someone not yet named.
chat-you = You
chat-someone = Someone ({ $tag })
# A workspace whose name has not arrived yet.
chat-workspace-fallback = Tilo workspace
chat-workspace-current-fallback = Current workspace

# A member's presence under their name in the roster.
chat-presence-online = Online
chat-presence-idle = Idle
chat-presence-dnd = Do not disturb
chat-presence-offline = Offline
chat-member-you = { $name } (you)
chat-member-agent = { $name } (agent)
chat-member-in-voice = 🔊 In voice · { $channel }

# The roster's heading: "Collaborators" once an agent is present.
chat-roster-people = People
chat-roster-collaborators = Collaborators
chat-roster-subtitle = { $agents ->
    [yes] { $count ->
        [one] { $count } collaborator · { $workspace }
       *[other] { $count } collaborators · { $workspace }
    }
   *[no] { $count ->
        [one] { $count } person · { $workspace }
       *[other] { $count } people · { $workspace }
    }
}

# Context-menu rows.
chat-menu-rename = Rename
chat-menu-delete = Delete
chat-menu-move-up = Move up
chat-menu-move-down = Move down
chat-menu-new-text-channel = New text channel
chat-menu-new-voice-channel = New voice channel
chat-menu-new-category = New category
chat-menu-toggle-collapse = Collapse / expand
chat-menu-archive = Archive / unarchive
chat-menu-edit-message = Edit message
chat-menu-pin-message = Pin / unpin
chat-menu-delete-message = Delete message

# Names of structure the app creates before somebody renames it.
chat-default-category = New Category
chat-default-voice-channel = Voice
chat-default-new-voice-channel = New Voice

# The inbox and the channel list header.
chat-inbox-title = Inbox
chat-inbox-subtitle = Conversations across Tilo workspaces
chat-sidebar-title = { $workspace } channels
chat-online-count = { $agents ->
    [yes] { $count ->
        [one] { $count } collaborator online
       *[other] { $count } collaborators online
    }
   *[no] { $count ->
        [one] { $count } person online
       *[other] { $count } people online
    }
}

# The conversation header.
chat-header-from = From { $source }
chat-header-source-topic = { $source } · { $topic }
chat-header-in-call = ● In call
chat-header-call = 📹 Call

# The timeline.
chat-jump-latest = ↓ Jump to latest
chat-channel-start = This is the start of #{ $channel }. Write the first message to begin.
chat-channel-empty = There are no messages in #{ $channel } yet.
chat-day-today = Today
chat-day-yesterday = Yesterday
chat-day-ago = { $days ->
    [one] { $days } day ago
   *[other] { $days } days ago
}
chat-edited-view-history = (edited) · view history
chat-replies = { $count ->
    [one] { $count } reply
   *[other] { $count } replies
}
chat-last-reply = last reply { $time }
chat-image-loading = Loading image…
chat-image-pending = Loading shared image…
chat-image-unavailable = Shared image unavailable
chat-image-decoding = Decoding image…
chat-video-meta = video · { $size }
chat-access-not-granted = Access not granted
chat-view-only = 🔒 View-only access to this workspace

# The inline message editor.
chat-edit-placeholder = Edit message…
chat-edit-hint = Enter to save · Esc to cancel
chat-edit-save = Save
chat-edit-cancel = Cancel

# The composers.
chat-composer-placeholder = Message #{ $channel }
chat-thread-composer-placeholder = Reply…

# The thread panel.
chat-thread-title = Thread
chat-thread-back = ← Back

# The attachment picker and what became of an attachment.
chat-attach-title = Share a file
chat-attach-empty = No attachments in { $dir }
chat-attach-more = +{ $count } more in the workspace
chat-attach-loading = Loading attachments…
chat-attach-preparing = Preparing attachment…
chat-attach-shared = Attachment shared
chat-attach-picker-exhausted = Attachment picker unavailable: request counter exhausted
chat-attach-picker-cancelled = Attachment picker cancelled
chat-attach-picker-failed = Attachment picker failed: { $reason }
chat-attach-import-exhausted = Attachment import unavailable: request counter exhausted
chat-attach-unsupported-no-transport = Attachment import unsupported: no host asset transport
chat-attach-import-stale = Attachment import discarded: stale result
chat-attach-import-failed = Attachment import failed: { $reason }
chat-attach-import-permission = Attachment import cancelled: permission changed
chat-attach-reason-workspace-unavailable = workspace is unavailable
chat-attach-reason-file-unavailable = file is unavailable
chat-attach-reason-not-shareable = file is not shareable or exceeds 32 MiB
chat-attach-reason-unreadable = could not read file
chat-attach-reason-empty = file is empty
chat-attach-reason-too-large = file exceeds 32 MiB
chat-attach-reason-store-refused = shared content store refused the attachment
chat-attach-reason-verification = shared byte verification failed
chat-attach-default-name = file

# The call: tiles, the floating panel and the stage.
chat-call-title-fallback = Call
chat-call-active = Active call
chat-call-expand = Expand
chat-call-camera-off = Camera off
chat-call-no-video = No video
chat-call-you = You
chat-call-you-host = You (host)
chat-call-session-peer = peer-to-peer
chat-call-session-host = host relay
# A call tile showing a verified remote audio stream.
chat-call-audio-stream = Audio stream
# A remote audio stream nobody has named, told apart by the end of its id.
chat-call-audio-stream-name = Audio stream { $tail }
# The call panel's strip: the verified audio streams, not a roster of people.
chat-call-panel-streams = { $caret } { $count ->
    [one] 1 audio stream
   *[other] { $count } audio streams
} · { $state ->
    [live] Connected
    [reconnecting] Reconnecting
    [joining] Joining
   *[unavailable] Unavailable
}
chat-call-headcount = { $count } in the call
chat-call-headcount-sharing = { $me ->
    [yes] { $count } in the call · You are sharing a screen
   *[no] { $count } in the call · { $name } is sharing a screen
}
chat-call-sharing-you = You're sharing your screen
chat-call-sharing-other = { $name } is sharing a screen
chat-call-sharing-you-sub = Everyone in the call sees your screen
chat-call-sharing-other-sub = Connecting to the shared screen…
chat-call-live = { $me ->
    [yes] Live · Your screen
   *[no] Live · { $name }'s screen
}
chat-call-action-join = Join call
chat-call-action-leave = Leave call
chat-call-action-speak = Use microphone
chat-call-action-camera = Use camera
chat-call-action-share = Share screen

# The emoji picker and a message's edit history.
chat-history-title = Edit history
chat-history-now = Now · { $time }
chat-history-edited = Edited · { $time }

# The app's accessibility tree: the window, its conversation and messages.
chat-a11y-app = Chat
chat-a11y-app-inbox = Inbox: conversations across Tilo workspaces
chat-a11y-app-channels = Channels in the current Tilo workspace
chat-a11y-messages = Messages
chat-a11y-message = { $author }: { $text }
chat-a11y-unknown-person = Unknown person

# Accessible names of the app's controls.
chat-a11y-inbox = { $unread ->
    [0] Inbox
   *[other] Inbox, { $unread } unread
}
chat-a11y-current-context = { $workspace } channels, current workspace
chat-a11y-channel = { $glyph } { $name }{ $inbox ->
        [yes] , from { $source }
       *[no] {""}
    }{ $unread ->
        [0] {""}
       *[other] , { $unread } unread
    }
chat-a11y-category = { $collapsed ->
    [yes] Expand { $name }
   *[no] Collapse { $name }
}
chat-a11y-category-fallback = category
chat-a11y-add-channel = Add channel
chat-a11y-add-category = Add category
chat-a11y-rename-field = Rename field
chat-a11y-edit-message = Edit message
chat-a11y-composer = Message composer
chat-a11y-thread-composer = Thread reply composer
chat-a11y-send = Send message
chat-a11y-jump-latest = Jump to latest message
chat-a11y-attach = Attach a workspace file
chat-a11y-attach-pick = Attach { $name }
chat-a11y-start-call = Start or open call
chat-a11y-show-navigation = Show conversation navigation
chat-a11y-show-detail = Show people or thread
chat-a11y-open-thread = Open message thread
chat-a11y-close-thread = Close thread
chat-a11y-edit-history = Show edit history
chat-a11y-edit-save = Save message edit
chat-a11y-edit-cancel = Cancel message edit
chat-a11y-call-mute = Mute microphone
chat-a11y-call-share = Share screen
chat-a11y-call-participants = Show call participants
chat-a11y-call-expand = Expand or restore call
chat-a11y-call-leave = Leave call

# Emoji picker: its title when inserting into the composer or reacting to a
# message, the search field's placeholder and the grid's section headings.
chat-emoji-title = Emoji
chat-emoji-add-reaction = Add reaction
chat-emoji-search = Search emoji
chat-emoji-recent = Recently used
chat-emoji-results = Results
chat-emoji-no-results = No emoji found

# What the edit history calls each change Chat makes.
chat-undo-send-message = Send message
chat-undo-edit-message = Edit message
chat-undo-delete-message = Delete message
chat-undo-add-reaction = Add reaction
chat-undo-remove-reaction = Remove reaction
chat-undo-pin-message = Pin message
chat-undo-unpin-message = Unpin message
chat-undo-update-agent-work = Update agent work
chat-undo-set-report-status = Set report status
chat-undo-withdraw-attachment = Withdraw attachment
chat-undo-make-feedback-channel = Make feedback channel
chat-undo-make-ordinary-channel = Make ordinary channel
chat-undo-create-channel = Create channel
chat-undo-create-category = Create category
chat-undo-rename-category = Rename category
chat-undo-delete-category = Delete category
chat-undo-collapse-category = Collapse category
chat-undo-expand-category = Expand category
chat-undo-move-channel-up = Move channel up
chat-undo-move-channel-down = Move channel down
chat-undo-rename-channel = Rename channel
chat-undo-delete-channel = Delete channel
chat-undo-archive-channel = Archive channel
chat-undo-unarchive-channel = Unarchive channel
chat-undo-mark-channel-read = Mark channel read
chat-undo-join-call = Join call
chat-undo-leave-call = Leave call
chat-undo-update-call-state = Update call state
chat-undo-start-presenting = Start presenting
chat-undo-stop-presenting = Stop presenting
chat-undo-update-call-connection = Update call connection
chat-undo-resource-message = Message

# Search: the sections Chat's results are grouped under, and their lines.
chat-search-group-conversations = Conversations
chat-search-group-messages = Messages
chat-search-group-people = People
chat-search-group-workspaces = Workspaces
chat-search-group-categories = Categories
chat-search-subtitle-workspace = Workspace
chat-search-subtitle-agent = Agent
chat-search-subtitle-person = Person
chat-search-category-collapsed = collapsed
chat-search-category-expanded = expanded
chat-search-attachment = Attachment

# The palette: what Chat's actions, their prompts and its resources are called.
chat-action-add-a-reference-from-this-room-to-the-attached-record-no-bytes-are-copied = Add a reference from this room to the attached record; no bytes are copied
chat-action-add-my-reaction = Add my reaction
chat-action-add-reaction = Add reaction
chat-action-add-the-feedback-channel = Add the feedback channel
chat-action-archive-channel = Archive channel
chat-action-attach-file = Attach file
chat-action-attachment = Attachment
chat-action-category-name = category name…
chat-action-channel-name = channel name…
chat-action-collapse-category = Collapse category
chat-action-delete-category = Delete category
chat-action-delete-channel = Delete channel
chat-action-delete-message = Delete message
chat-action-edit-message = Edit message
chat-action-emoji = Emoji
chat-action-expand-category = Expand category
chat-action-file-path = File path
chat-action-invite-this-person-into-the-current-workspace = Invite this person into the current workspace
chat-action-invite-to-workspace = Invite to workspace
chat-action-leave-call = Leave call
chat-action-make-a-feedback-channel = Make a feedback channel
chat-action-make-an-ordinary-channel = Make an ordinary channel
chat-action-mark-as-read = Mark as read
chat-action-message = Message
chat-action-name = Name
chat-action-new-category = New Category
chat-action-new-channel = New channel
chat-action-new-text = new text…
chat-action-open-call-conversation = Open call conversation
chat-action-open-category = Open category
chat-action-open-conversation = Open conversation
chat-action-open-person = Open person
chat-action-open-referenced-item = Open referenced item
chat-action-open-the-conversation-only-the-two-of-you-are-in = Open the conversation only the two of you are in
chat-action-open-what-this-message-is-about-in-the-app-it-belongs-to = Open what this message is about in the app it belongs to
chat-action-open-workspace = Open workspace
chat-action-people = People
chat-action-pin-message = Pin message
chat-action-post-a-remark = Post a remark
chat-action-post-message = Post message
chat-action-remove-my-reaction = Remove my reaction
chat-action-remove-my-vote = Remove my vote
chat-action-remove-this-room-s-reference-members-who-already-synced-it-keep-their-copy = Remove this room's reference; members who already synced it keep their copy
chat-action-rename-category = Rename category
chat-action-rename-channel = Rename channel
chat-action-replay-recording = Replay recording
chat-action-replay-this-recorded-session-in-a-sandboxed-replay-room = Replay this recorded session in a sandboxed replay room
chat-action-reply = Reply
chat-action-reply-in-thread = Reply in thread
chat-action-reveal-message = Reveal message
chat-action-reveal-shared-message = Reveal shared message
chat-action-set-report-status = Set report status
chat-action-share-record-into-this-room = Share record into this room
chat-action-show-author = Show author
chat-action-sort-and-filter-reports = Sort and filter reports
chat-action-stop-sharing-record-with-this-room = Stop sharing record with this room
chat-action-the-times-contradict-the-prose = the times contradict the prose
chat-action-type-a-message = type a message… (@Name mentions a member)
chat-action-type-a-reply = type a reply…
chat-action-unarchive-channel = Unarchive channel
chat-action-unpin-message = Unpin message
chat-action-vote-for-this-report = Vote for this report
chat-action-withdraw-from-this-room = Withdraw from this room
chat-action-workspace = Workspace
chat-action-call-resource = Call: { $channel }
chat-action-call-status = Call status
chat-action-input = Input
chat-action-input-from-call-status = Input from call status
chat-action-join-muted = Join muted
chat-action-mute-microphone = Mute microphone
chat-action-request-microphone = Request microphone
chat-action-select-microphone = Select microphone

# The emoji picker and reaction chips.
chat-emoji-insert = Insert emoji
chat-emoji-group = Emoji group
# A picker cell; the name is the emoji's name in the desktop's language.
chat-emoji-choose = Choose { $name }
# A reaction chip under a message, which adds or removes the user's own.
chat-reaction-add = Add { $emoji } reaction
chat-reaction-remove = Remove { $emoji } reaction
