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
chat-thread-start = Thread start
chat-thread-end = Latest reply
chat-thread-messages = Thread messages
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
chat-agent-kind = Agent
chat-agents-section = Agents
chat-agent-message-placeholder = Work with { $name }
chat-agent-empty-queue-title = Start something with { $name }
chat-agent-empty-queue-message = What would you like to work on? Save a task for this agent, then choose Start task when you’re ready.
chat-agent-empty-steer-title = Direct { $name }’s current task
chat-agent-empty-steer-message = Ask, clarify, or change course. Every message steers the selected task and run.
chat-agent-empty-unavailable-title = { $name } is unavailable
chat-agent-float = Float conversation
chat-agent-return = Return conversation
chat-agent-drag = Drag conversation
chat-agent-moving-title = Moving { $name }’s conversation
chat-agent-moving-message = The conversation will appear in its own window when the handoff finishes.
chat-agent-detached-title = { $name }’s conversation is in another window
chat-agent-detached-message = Use Return conversation to bring the same draft back here.
chat-agent-mode-queue = New task
chat-agent-mode-steer = This task
chat-agent-mode-steer-unavailable = This task
chat-agent-task-placeholder = Describe the next task
chat-agent-steer-placeholder = Ask, clarify, or change course…
chat-agent-send-queue = Save task
chat-agent-send-steer = Send
chat-conversation-read-only = You can read this conversation but not write in it.
chat-agent-unavailable = This agent is no longer available here. Your draft is kept on this device.
chat-agent-task-loading = Loading current task…
chat-agent-task-none = No current task
chat-agent-task-queued = Ready to start
chat-agent-task-saved = Task saved — nothing runs until you choose Start task
chat-agent-task-unavailable = Current task unavailable
chat-agent-task-choose = Choose a current task ({ $count } available)
chat-agent-task-more = { $count ->
    [one] 1 more current task
   *[other] { $count } more current tasks
}
chat-agent-task-next = Show more current tasks
chat-agent-task-first = Back to first current tasks
chat-agent-task-phase-unknown = Status unknown
chat-agent-task-accepted = Agent accepted task
chat-agent-task-planning = Planning
chat-agent-task-executing = Working
chat-agent-task-started = Start approved
chat-agent-task-stopped = Stop requested
chat-agent-task-complete = Completed
chat-agent-task-cancelled = Cancelled
chat-agent-task-attention = Needs attention
chat-agent-direction-saving = Saving direction…
chat-agent-direction-saved = Saved — waiting for the agent to apply it
chat-agent-direction-applied = Applied — the agent is using your direction
chat-agent-direction-refused = Direction was not saved. Your draft is kept.
chat-agent-direction-outcome-unknown = Direction sent; waiting for Task Center to confirm the outcome
chat-agent-task-status = { $title }
    { $state } · Run { $run }
chat-agent-task-output = Work update: { $text }
chat-agent-task-author = { $name } (agent)
chat-agent-task-window = { $title } · Run { $run } · { $name }
chat-agent-task-selected-unavailable = { $task } · Run { $run } · Refresh or select the current run to steer
chat-agent-open-task = Open { $title } in Task Center
chat-agent-open-current-task = Open current task in Task Center
chat-agent-open-task-center = Open Task Center
chat-agent-start = Start task
chat-agent-start-unavailable = Select a saved task that is ready to start.
chat-agent-stop = Stop
chat-agent-select-task = Select task { $task }
chat-agent-stop-current = Stop current task
chat-resource-link-copied = Link copied
chat-resource-link-unavailable = Link unavailable
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
chat-post-file = Posted the file { $name }
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
