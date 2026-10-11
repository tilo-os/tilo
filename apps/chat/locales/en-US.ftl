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
chat-navigation-show = Show conversation navigation
chat-navigation-hide = Hide conversation navigation
chat-agent-kind = Agent
chat-agents-section = Agents
chat-agent-message-placeholder = Message { $name }
chat-agent-empty-title = Message { $name }
chat-agent-empty-message = Send a message without creating a task.
chat-agent-empty-queue-title = Queue a task for { $name }
chat-agent-empty-queue-message = Describe work to run after the current task.
chat-agent-empty-steer-title = Direct { $name }’s current task
chat-agent-empty-steer-message = Add direction without starting a new task.
chat-agent-empty-steer-stopping = Steering is unavailable while this task is stopping.
chat-agent-empty-unavailable-title = { $name } is unavailable
chat-agent-empty-read-only-title = Read-only conversation with { $name }
chat-agent-roster-label = { $name }, agent
chat-agent-float = Float conversation
chat-agent-return = Return conversation
chat-agent-drag = Drag conversation
chat-agent-moving-title = Moving { $name }’s conversation
chat-agent-moving-message = The conversation will appear in its own window when the handoff finishes.
chat-agent-detached-title = { $name }’s conversation is in another window
chat-agent-detached-message = Use Return conversation to bring the same draft back here.
chat-agent-mode-message = Message
chat-agent-mode-queue = Queue
chat-agent-mode-steer = Steer
chat-agent-mode-steer-unavailable = Steer unavailable
chat-agent-intent-queue = Queue task
chat-agent-intent-steer = Steer current task
chat-agent-composer-message = Message composer
chat-agent-composer-queue = Task instruction composer
chat-agent-composer-steer = Task direction composer
chat-agent-task-placeholder = Describe the task
chat-agent-steer-placeholder = Add direction for the current task
chat-agent-send-message = Send message
chat-agent-send-queue = Queue task
chat-agent-send-steer = Save direction
chat-agent-message-send-failed = Message was not sent. Your draft is still here.
chat-agent-queue-send-failed = Task was not queued. Your instruction is still here.
chat-agent-steer-send-failed = Direction was not saved. Your draft is still here.
chat-agent-message-awaiting-confirmation = Message sent. Still awaiting confirmation.
chat-conversation-read-only = You can read this conversation but not write in it.
chat-agent-message-read-only = You cannot send messages in this conversation.
chat-agent-unavailable = This agent is no longer available here. Your draft is kept on this device.
chat-agent-message-plain-only = Agent messages currently support plain text only. Remove formatting or attachments to send.
chat-agent-progress-waiting = Waiting for reply
chat-agent-progress-publishing = Publishing reply
chat-agent-progress-saved = Reply saved; waiting for conversation
chat-agent-progress-needs-attention = Reply needs attention
chat-agent-progress-unavailable = Last confirmed: { $status }. Status unavailable.
chat-agent-progress-unavailable-empty = Reply status unavailable
chat-agent-progress-refresh = Refresh reply status
chat-agent-task-loading = Loading current task…
chat-agent-task-none = No current task
chat-agent-task-unavailable = Current task unavailable
chat-agent-task-changed = Task changed — confirm a current task
chat-agent-task-choose = Choose a current task ({ $count } available)
chat-agent-task-none-last = No current task — last task: { $title }
chat-agent-task-incomplete = Current task list incomplete
chat-agent-task-incomplete-last = Task list incomplete — last confirmed: { $title }
chat-agent-task-unavailable-last = Current tasks unavailable — last confirmed: { $title }
chat-agent-task-phase-unknown = Status unknown
chat-agent-task-accepted = Agent accepted task
chat-agent-task-planning = Planning
chat-agent-task-executing = Executing
chat-agent-task-stopping = Stop requested
chat-agent-task-attention = Needs attention
chat-agent-direction-saved = Direction saved
chat-agent-direction-applied = Direction applied
chat-agent-direction-outcome-unknown = Direction sent; waiting for Task Center to confirm the outcome
chat-agent-task-status = { $state }: { $title }
chat-agent-task-queued-count = { $count ->
    [one] 1 queued task
   *[other] { $count } queued tasks
}
chat-agent-open-task = Open { $title } in Task Center
chat-agent-open-current-task = Open current task in Task Center
chat-agent-open-task-center = Open Task Center
chat-agent-stop = Stop
chat-agent-select-task = Select task { $task }
chat-agent-stop-current = Stop current task
chat-voice-unavailable = Voice calls are unavailable until a media service is connected.
chat-attachment-import-unavailable = Attachment import is unavailable on this host.

# "You" belongs to the current reader; shared/legacy names identify their person.
chat-person-unnamed = Someone ({ $tag })
chat-person-relative-name = { $name } ({ $tag })
