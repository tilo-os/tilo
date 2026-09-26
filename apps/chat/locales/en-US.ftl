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
