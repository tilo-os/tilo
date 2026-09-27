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

# Emoji picker: its title when inserting into the composer or reacting to a
# message, the search field's placeholder and the grid's section headings.
chat-emoji-title = Emoji
chat-emoji-add-reaction = Add reaction
chat-emoji-search = Search emoji
chat-emoji-recent = Recently used
chat-emoji-results = Results
chat-emoji-no-results = No emoji found
chat-emoji-insert = Insert emoji
chat-emoji-group = Emoji group
# A picker cell; the name is the emoji's name in the desktop's language.
chat-emoji-choose = Choose { $name }
# A reaction chip under a message, which adds or removes the user's own.
chat-reaction-add = Add { $emoji } reaction
chat-reaction-remove = Remove { $emoji } reaction

# Right-click / long-press menu on a channel, category, workspace or one of
# the user's own messages.
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

# Placeholders of the empty message composer and the thread's reply field.
chat-composer-placeholder = Message #{ $channel }
chat-composer-thread-placeholder = Reply…

# How participants are named on screen: the user's own lines, a native audio
# stream (the tail of its id), someone the room has not named yet (a short tag
# that tells two of them apart) and a message author that is not known.
chat-identity-you = You
chat-identity-audio-stream = Audio stream { $tail }
chat-identity-someone = Someone ({ $tag })
chat-identity-unknown-person = Unknown person
# Title of the members panel: only people, or people and agents together.
chat-identity-people = People
chat-identity-collaborators = Collaborators

# Names a new workspace and its first categories and channels start with.
chat-default-workspace = My Workspace
chat-default-text-category = Channels
chat-default-voice-category = Voice
chat-default-text-channel = general
chat-default-voice-channel = Lounge
# Names of a channel or category created from the sidebar or its menu, before
# the user renames it, and of the voice channel the header's call button
# creates when the workspace has none.
chat-default-new-category = New Category
chat-default-new-text-channel = new-channel
chat-default-new-voice-channel = New Voice
chat-default-call-channel = Voice
# An agent taking part without a name of its own, and a Ctrl+K result for a
# message that has neither text nor an attachment.
chat-default-agent = Agent
chat-default-no-text = (no text)

# Screen-reader names of the sidebar's destinations.
chat-a11y-inbox =
    { $unread ->
        [0] Inbox
        [one] Inbox, { $unread } unread
       *[other] Inbox, { $unread } unread
    }
chat-a11y-current-context = { $workspace } channels, current workspace
chat-a11y-current-workspace = Current workspace
# A channel row; in the inbox it also names the workspace it comes from.
chat-a11y-channel =
    { $unread ->
        [0] { $name }
        [one] { $name }, { $unread } unread
       *[other] { $name }, { $unread } unread
    }
chat-a11y-channel-from =
    { $unread ->
        [0] { $name }, from { $source }
        [one] { $name }, from { $source }, { $unread } unread
       *[other] { $name }, from { $source }, { $unread } unread
    }
chat-a11y-fallback-workspace = Tilo workspace
# The caret beside a category heading.
chat-a11y-category-expand = Expand { $name }
chat-a11y-category-collapse = Collapse { $name }
chat-a11y-category-fallback = category
chat-a11y-add-channel = Add channel
chat-a11y-add-category = Add category
chat-a11y-rename-field = Rename field

# Screen-reader names of the conversation's controls.
chat-a11y-edit-message = Edit message
chat-a11y-composer = Message composer
chat-a11y-thread-composer = Thread reply composer
chat-a11y-send = Send message
chat-a11y-jump-latest = Jump to latest message
chat-a11y-attach = Attach a workspace file
chat-a11y-attach-file = Attach { $file }
chat-a11y-start-call = Start or open call
chat-a11y-show-navigation = Show conversation navigation
chat-a11y-show-details = Show people or thread
chat-a11y-open-thread = Open message thread
chat-a11y-close-thread = Close thread
chat-a11y-edit-history = Show edit history
chat-a11y-edit-save = Save message edit
chat-a11y-edit-cancel = Cancel message edit
chat-a11y-messages = Messages
chat-a11y-inbox-description = Inbox: conversations across Tilo workspaces
chat-a11y-channels-description = Channels in the current Tilo workspace

# Call controls and the call actions a permission check names; the active
# call's title when it is dragged out into its own floating window.
chat-a11y-mute-microphone = Mute microphone
chat-a11y-share-screen = Share screen
chat-a11y-call-participants = Show call participants
chat-a11y-call-expand = Expand or restore call
chat-a11y-leave-call = Leave call
chat-a11y-join-call = Join call
chat-a11y-use-microphone = Use microphone
chat-a11y-use-camera = Use camera
chat-a11y-active-call = Active call

# Status line in the attachment picker. The reason is one of the
# chat-error-* attachment reasons below.
chat-status-picker-exhausted = Attachment picker unavailable: request counter exhausted
chat-status-picker-cancelled = Attachment picker cancelled
chat-status-picker-failed = Attachment picker failed: { $reason }
chat-status-import-exhausted = Attachment import unavailable: request counter exhausted
chat-status-attachment-preparing = Preparing attachment…
chat-status-import-stale = Attachment import discarded: stale result
chat-status-import-failed = Attachment import failed: { $reason }
chat-status-import-permission = Attachment import cancelled: permission changed
chat-status-attachment-shared = Attachment shared
# Why an attachment could not be read or shared.
chat-error-workspace-unavailable = workspace is unavailable
chat-error-file-unavailable = file is unavailable
chat-error-file-not-shareable = file is not shareable or exceeds 32 MiB
chat-error-file-unreadable = could not read file
chat-error-file-empty = file is empty
chat-error-file-too-large = file exceeds 32 MiB
chat-error-content-store-refused = shared content store refused the attachment
chat-error-verification-failed = shared byte verification failed

# Notice in the call area when voice is unavailable or has stopped.
chat-status-audio-only = This call supports microphone audio only
chat-status-voice-unavailable = Voice is unavailable
chat-status-call-denied = Call access was denied
chat-status-mic-permission-denied = Microphone permission was denied
chat-status-mic-disconnected = Microphone disconnected
chat-status-mic-failed = Microphone could not start
chat-status-call-disconnected = Call disconnected
chat-status-call-ended = Call ended
chat-status-call-expired = Call session expired
chat-error-media-unsupported = Calls and screen sharing are unsupported until a media transport is connected
chat-error-voice-channel-writer = A room writer must create a voice channel

# Errors shown when a call control, a Ctrl+K action or a link into Chat
# cannot be carried out.
chat-error-call-invalid = Invalid call command
chat-error-call-busy = Call controls are busy
chat-error-call-exhausted = Call control sequence exhausted
chat-error-voice-channel-gone = Voice channel no longer exists
chat-error-call-not-permitted = Call action is not permitted
chat-error-call-not-active = This is not the active call
chat-error-call-needs-effect = Call controls require a valid local Effect
chat-error-navigation-needs-view = Chat navigation requires a View operation
chat-error-workspace-gone = Workspace no longer exists
chat-error-category-gone = Category no longer exists
chat-error-conversation-gone = Conversation no longer exists
chat-error-message-gone = Message no longer exists
chat-error-person-gone = Person no longer exists
chat-error-attachment-unsupported = Attachment import unsupported: no host asset transport
chat-error-attachment-needs-effect = Attachment import requires an Effect operation
chat-error-attachment-path-empty = Attachment path is empty
chat-error-attachment-not-accepted = Attachment import was not accepted
# Opening a Chat record that is not a Chat workspace.
chat-error-no-workspace-schema = Chat record has no workspace schema

# Presence state under a member in the People roster.
chat-presence-online = Online
chat-presence-idle = Idle
chat-presence-dnd = Do not disturb
chat-presence-offline = Offline
# Roster status of a member who is in a voice channel; the channel is its name.
chat-presence-in-voice-channel = In voice · { $channel }
# Pill beside the workspace title counting who is online. The collaborator
# form is used when the workspace also has agents in it.
chat-presence-people-online =
    { $count ->
        [one] { $count } person online
       *[other] { $count } people online
    }
chat-presence-collaborators-online =
    { $count ->
        [one] { $count } collaborator online
       *[other] { $count } collaborators online
    }

# Relative day on the date divider above the first message.
chat-time-today = Today
chat-time-yesterday = Yesterday
chat-time-days-ago =
    { $count ->
        [one] { $count } day ago
       *[other] { $count } days ago
    }

# Header of the cross-workspace Inbox in the sidebar.
chat-view-inbox-title = Inbox
chat-view-inbox-subtitle = Conversations across Tilo workspaces
# Conversation header topic while the Inbox is open: which workspace the
# channel comes from, and the name used when that workspace is unknown.
chat-view-inbox-source = From { $workspace }
chat-view-inbox-source-fallback = Tilo workspace
# Sidebar header above a workspace's channel list.
chat-view-workspace-channels = { $workspace } channels
# Floating button that scrolls the conversation back to the newest message.
chat-view-jump-latest = Jump to latest
# Empty channel: the channel is its "#name". The first line is shown to someone
# who may post, the second to someone who may only read.
chat-view-channel-start = This is the start of { $channel }. Write the first message to begin.
chat-view-channel-empty = There are no messages in { $channel } yet.
# Link under an edited message that opens its edit history.
chat-view-edited-history = (edited) · view history
# Inline editor for a sent message: placeholder, keyboard hint and buttons.
chat-view-edit-placeholder = Edit message…
chat-view-edit-hint = Enter to save · Esc to cancel
chat-view-edit-save = Save
chat-view-edit-cancel = Cancel
# Reply count on a message's thread bar and in the thread panel's divider.
chat-view-replies =
    { $count ->
        [one] { $count } reply
       *[other] { $count } replies
    }
# Right side of a message's thread bar; the time is a clock time.
chat-view-last-reply = last reply { $time }
# Banner in place of the composer for someone with read-only access.
chat-view-read-only-banner = View-only access to this workspace
# Thread panel title, and the button that returns from it on a narrow window.
chat-view-thread-title = Thread
chat-view-back = Back
# Headcount under the People panel title, followed by the workspace name. The
# collaborator form is used when the workspace also has agents in it.
chat-view-roster-people =
    { $count ->
        [one] { $count } person
       *[other] { $count } people
    }
chat-view-roster-collaborators =
    { $count ->
        [one] { $count } collaborator
       *[other] { $count } collaborators
    }
# A member's name in the People panel when it is the user or an agent.
chat-view-member-you = { $name } (you)
chat-view-member-agent = { $name } (agent)

# Status of an image attachment while its picture is not shown yet.
chat-attachment-image-loading = Loading image…
chat-attachment-image-pending = Loading shared image…
chat-attachment-image-unavailable = Shared image unavailable
chat-attachment-image-decoding = Decoding image…
# Video card detail when the video's dimensions are unknown; size is a byte size.
chat-attachment-video-meta = video · { $size }
# Attachment placeholder for someone not allowed to view the file.
chat-attachment-access-denied = Access not granted
# Workspace file picker above the composer: title, loading state, empty state
# (the folder is a filesystem path) and the footer for entries not listed.
chat-attachment-picker-title = Share a file
chat-attachment-loading = Loading attachments…
chat-attachment-picker-empty = No attachments in { $folder }
chat-attachment-picker-more = +{ $count } more in the workspace

# Conversation header button that starts a call, or shows one is running.
chat-call-start = Call
chat-call-in-call = In call
# Call title when its channel cannot be found.
chat-call-title-fallback = Call
# Name plate on the user's own call tile, and on it as the call's host.
chat-call-you = You
chat-call-you-host = You (host)
# Caption on a call tile with no picture: camera switched off, or no video
# reaching this device.
chat-call-camera-off = Camera off
chat-call-no-video = No video
# Caption on a tile that carries only a verified audio stream.
chat-call-audio-stream = Audio stream
# Floating call panel: stream count and the connection state beside it.
chat-call-audio-streams =
    { $count ->
        [one] { $count } audio stream
       *[other] { $count } audio streams
    }
chat-call-state-connected = Connected
chat-call-state-reconnecting = Reconnecting
chat-call-state-joining = Joining
chat-call-state-unavailable = Unavailable
# Floating call panel button that opens the expanded call view.
chat-call-expand = Expand
# Expanded call subtitle: how many people are in the call.
chat-call-headcount = { $count } in the call
# Who is sharing a screen, in the call subtitle and on the shared-screen tile.
chat-call-sharing-self = You're sharing your screen
chat-call-sharing-other = { $name } is sharing a screen
# Detail under the shared-screen heading: for the sharer, and for viewers while
# the first frame is on its way.
chat-call-sharing-self-detail = Everyone in the call sees your screen
chat-call-sharing-connecting = Connecting to the shared screen…
# Pill on the featured shared-screen tile.
chat-call-live-own-screen = Live · Your screen
chat-call-live-screen = Live · { $name }'s screen
# Edit history popover: its title and each version's heading (time is a clock time).
chat-call-history-title = Edit history
chat-call-history-now = Now · { $time }
chat-call-history-edited = Edited · { $time }

# What a change did, as the room's history lists it (also the Ctrl+K titles of
# the matching message, channel and category actions).
chat-action-send-message = Send message
chat-action-edit-message = Edit message
chat-action-delete-message = Delete message
chat-action-remove-reaction = Remove reaction
chat-action-pin-message = Pin message
chat-action-unpin-message = Unpin message
chat-action-update-agent-work = Update agent work
chat-action-create-channel = Create channel
chat-action-create-category = Create category
chat-action-rename-category = Rename category
chat-action-delete-category = Delete category
chat-action-collapse-category = Collapse category
chat-action-expand-category = Expand category
chat-action-move-channel-up = Move channel up
chat-action-move-channel-down = Move channel down
chat-action-rename-channel = Rename channel
chat-action-delete-channel = Delete channel
chat-action-archive-channel = Archive channel
chat-action-unarchive-channel = Unarchive channel
chat-action-mark-channel-read = Mark channel read
chat-action-join-call = Join call
chat-action-leave-call = Leave call
chat-action-update-call-state = Update call state
chat-action-start-presenting = Start presenting
chat-action-stop-presenting = Stop presenting
chat-action-update-call-connection = Update call connection
# The name a history entry gives the message a change touched.
chat-action-message = Message

# Name recorded beside a message from an agent that has no display name.
chat-action-agent-default-name = Agent

# Ctrl+K and search labels for a channel or workspace whose name is blank.
chat-action-unnamed-channel = channel
chat-action-workspace = Workspace

# Ctrl+K actions that open or reveal something without changing it.
chat-action-open-person = Open person
chat-action-open-workspace = Open workspace
chat-action-open-category = Open category
chat-action-open-conversation = Open conversation
chat-action-open-call-conversation = Open call conversation
chat-action-reveal-message = Reveal message
chat-action-reveal-shared-message = Reveal shared message

# Ctrl+K actions on a workspace that create a channel or a category, the
# prompt's field label and placeholder, and the name used when it is left blank.
chat-action-new-channel = New channel
chat-action-new-category = New category
chat-action-param-name = Name
chat-action-channel-name-placeholder = channel name…
chat-action-category-name-placeholder = category name…
chat-action-new-channel-default-name = new-channel
chat-action-new-category-default-name = New Category

# Ctrl+K actions on a text channel: posting (with its field label and
# placeholder), attaching a file by path, and marking it read.
chat-action-post-message = Post message
chat-action-param-message = Message
chat-action-post-message-placeholder = type a message… (@Name mentions a member)
chat-action-attach-file = Attach file
chat-action-param-file-path = File path
chat-action-mark-as-read = Mark as read

# Ctrl+K entry for a voice channel's call; the channel is its `🔊 name` label.
chat-action-call-label = Call: { $channel }
# Ctrl+K actions on a call.
chat-action-join-muted = Join muted
chat-action-mute-microphone = Mute microphone
chat-action-request-microphone = Request microphone
chat-action-call-status = Call status
chat-action-select-microphone = Select microphone
chat-action-param-input = Input
chat-action-select-microphone-placeholder = Input from call status

# Ctrl+K actions on a message and its reactions.
chat-action-edit-message-placeholder = new text…
chat-action-remove-my-reaction = Remove my reaction
chat-action-add-my-reaction = Add my reaction
# Ctrl+K and search title of a message that has only an attachment.
chat-action-attachment = Attachment

# Group headings in global search.
chat-search-kind-conversations = Conversations
chat-search-kind-messages = Messages
chat-search-kind-people = People
chat-search-kind-workspaces = Workspaces
chat-search-kind-categories = Categories
# Search subtitle of a category, after its workspace's name.
chat-search-category-collapsed = collapsed
chat-search-category-expanded = expanded
# Search subtitle of a participant.
chat-search-subtitle-agent = Agent
chat-search-subtitle-person = Person

# Ctrl+K action that reads a conversation's messages page by page, its
# placeholder and parameter descriptions (pageSize and nextCursor are field
# names, not words), and why it is currently unavailable.
chat-read-messages = Read conversation messages
chat-read-messages-placeholder = pageSize and optional cursor
chat-read-param-page-size = Maximum items in this page
chat-read-param-cursor = Opaque nextCursor from the preceding page
chat-read-unavailable = A current committed document read is not available
