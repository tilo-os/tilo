# Chat — user-facing strings.

# Marker beside a participant in the `@` picker that is an agent rather than a
# person, so somebody can tell at a glance that they are addressing software.
chat-mention-agent-tag = agent

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

# Governed voice status, shared by native and browser hosts.
chat-call-issue-unavailable = Voice is unavailable
chat-call-issue-denied = Call access was denied
chat-call-issue-permission-denied = Microphone permission was denied
chat-call-issue-source-lost = Microphone disconnected
chat-call-issue-capture-failed = Microphone could not start
chat-call-issue-transport-failed = Call disconnected
chat-call-issue-session-ended = Call ended
chat-call-issue-stale = Call session expired

chat-call-title = Call: { $channel }
chat-call-no-workspace = Open a workspace to call in
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
chat-call-reconnecting = Reconnecting
chat-call-joining = Joining
chat-call-voice-unavailable = Voice unavailable

# An attachment the reader has no access to: the card says why it cannot open.
chat-attachment-no-access = You don't have access to this file
chat-call-unavailable = Unavailable
chat-call-stream-status = { $count } audio streams · { $status }

# Effects on the Chat root that start a call in the active workspace (Home's
# "Plan a meeting", the palette, agents). Video is published but unavailable.
chat-start-call = Start a call
chat-start-video-call = Start a video call
chat-start-video-call-unavailable = Calls are voice-only for now; video is not supported yet

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
chat-thread-title = Thread
chat-thread-back = Back
chat-thread-close = Close thread
chat-thread-replies = { $count ->
    [one] 1 reply
   *[other] { $count } replies
    }

chat-context-sharing = Sharing live Chat context
chat-context-following = Following live Chat context · Stop following in workspace controls
chat-context-stop = Stop sharing
chat-header-call-open = Open call
chat-header-voice-call = Voice call

chat-identity-available = Available
chat-identity-in-voice = In voice

chat-resource-copy-link = Copy resource link
chat-resource-open-link = Open resource in browser
chat-voice-unavailable = Voice calls are unavailable until a media service is connected.
chat-attachment-import-unavailable = Attachment import is unavailable on this host.

# "You" belongs to the current reader; shared/legacy names identify their person.
chat-person-unnamed = Someone ({ $tag })
chat-person-relative-name = { $name } ({ $tag })

## History: what each change did, by verb and by field
chat-begin-chat = Started the chat
chat-introduce = Introduced themselves as { $name }
chat-name-agent = Named an agent { $name }
chat-create-workspace = Created the workspace { $name }
chat-create-category = Created the category { $name } in { $item }
chat-create-channel = Created the channel #{ $name } in { $item }
chat-delete-category = Deleted the category { $item }
chat-move-channel = { $up ->
    [true] Moved #{ $item } up
   *[false] Moved #{ $item } down
}
chat-delete-channel = Deleted the channel #{ $item }
chat-start-draft = Started a draft
chat-post = Posted: { $text }
chat-share-resource = Shared a link in #{ $item }
chat-edit-message = Edited a message: { $text }
chat-delete-message = Deleted a message
chat-react = Reacted with { $emoji }
chat-unreact = Took back a { $emoji } reaction
chat-mark-read = Marked #{ $item } read
chat-record-work = Recorded what it is doing
chat-submit-feedback = Filed feedback in #{ $item }
chat-triage = Triaged feedback
chat-chat-workspaces = { $op ->
    [insert] Added a workspace
    [remove] Removed a workspace
   *[other] Reordered the workspaces
}
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
chat-chat-messages = { $op ->
    [insert] Added a message
    [remove] Removed a message
   *[other] Moved a message
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
chat-chat-drafts = { $op ->
    [splice] Typed in a draft
    [mark] Formatted a draft
    [replace] Cleared a draft
   *[other] Edited a draft
}
chat-workspace-name = Renamed the workspace { $item } to { $new }
chat-workspace-glyph = Changed the monogram of { $item } to { $new }
chat-workspace-color = Changed the colour of { $item }
chat-category-name = Renamed the category { $item } to { $new }
chat-channel-name = Renamed #{ $item } to #{ $new }
chat-channel-topic = Set the topic of #{ $item } to { $new }
chat-channel-category = Moved #{ $item } to another category
chat-channel-archived = { $new ->
    [true] Archived #{ $item }
   *[false] Restored #{ $item }
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
