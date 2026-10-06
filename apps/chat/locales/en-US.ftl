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
