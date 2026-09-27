# Continuing on this desktop what you were just doing on another device.
continuity-continue = Continue { $app } from { $device }
continuity-continue-subtitle = Focus the tab you were just using on { $device }
continuity-dismiss = Dismiss
continuity-dismiss-title = Dismiss “Continue { $app } from { $device }”
continuity-dismiss-subtitle = Hidden until you switch tabs on { $device } again
continuity-other-device = your other device

# Undoing a tab close.
tab-closed-toast = Closed { $title }
tab-closed-group-toast = Closed { $count } tabs
tab-closed-undo = Undo
tab-closed-undo-hint = Select to undo, or press { $chord } later
tab-closed-undo-hint-unbound = Select to undo

# Display layout sheet (Super+P)
display-sheet-title = Display
display-sheet-subtitle = Choose where your desktop appears. Super+P moves to the next choice.
display-sheet-a11y-description = Up and Down move between choices, Enter applies the highlighted one, Super+P moves to the next layout, Escape closes.
display-mode-extend = Extend
display-mode-extend-detail = One desktop across every display
display-mode-mirror = Mirror
display-mode-mirror-detail = The same picture on every display
display-mode-builtin-only = Only this screen
display-mode-builtin-only-detail = External displays turn off
display-mode-external-only = Only external
display-mode-external-only-detail = The built-in screen turns off
display-mode-current = Current
display-mode-unavailable-single = Connect another display to use this
display-mode-unavailable-builtin = This device has no built-in screen
display-mode-unavailable-mirror = These displays share no resolution
display-confirm-title = Keep these display settings?
display-confirm-countdown = Reverting in { $seconds } s
display-confirm-applying = Applying…
display-confirm-keep = Keep
display-confirm-revert = Revert
display-rejected = Couldn't apply this layout: { $reason }

# Keep or revert a change a display tool (kanshi, wlr-randr) made
display-confirm-tool-title = Keep the display settings { $tool } applied?
display-confirm-tool-unknown-title = Keep the display settings a tool applied?
display-confirm-always = Always keep changes from { $tool }
display-confirm-a11y-description = A display tool changed your screens. Enter keeps the change, Escape restores the previous layout.

# Wireless receivers, the lower section of the same sheet
wireless-heading = Wireless displays
wireless-share-audio = Share audio
wireless-stream = Stream
wireless-stream-auto = Auto
wireless-stream-fixed = Fixed
wireless-stream-fps = { $fps } fps
wireless-credential-label = Receiver PIN or password
wireless-credential-placeholder = PIN or receiver password
wireless-credential-hint = Leave blank unless the receiver asks. Enter connects.
wireless-scanning = Looking for displays…
wireless-scanning-detail = Keep the receiver awake and on the same network.
wireless-scan-failed = Couldn't scan
wireless-none = No displays found
wireless-none-detail = Try Refresh, or check Wi-Fi and the receiver's casting mode.
wireless-connected = Connected
wireless-connecting = Connecting…
wireless-connect = Connect
wireless-stop = Stop
wireless-wait = Wait
wireless-protocols = Miracast · Google Cast · AirPlay · FCast

# Identify displays: the card painted on each display
display-identify-builtin = Built-in display
display-identify-unnamed = Display { $number }

# The remark surface: say something about any part of any app, pick mode,
# recording and review before sending.

## Composer

remark-title = Say something about this
remark-title-crash = Tell us what happened
remark-close = Close
remark-about = About
remark-pick-more = Pick
remark-pick-more-description = Pick more things in any app
remark-remove-pick = Remove { $label }
remark-whole-app = { $app }, the whole app
remark-text-label = Remark
remark-placeholder-feedback = What happened, and what did you expect?
remark-placeholder = Write a remark
remark-community-label = Tilo Community #feedback
remark-destination = Send to
remark-dest-team-detail = Private · only you and the Tilo team
remark-dest-community-detail = Public · anyone in the community can read and reply
remark-dest-community-join = Public · sending joins the Tilo Community
remark-dest-room-detail = Everyone in this workspace
remark-dest-person-detail = Private · only you and { $name }
remark-dest-agent-detail = Private · { $name } is an agent
remark-retraction = You can withdraw it later, but anyone who already received it keeps their copy.
remark-show-what-happened = Show what happened
remark-record-now = Record from now
remark-record-now-detail = The Recorder joins as an agent until you stop
remark-use-recent = Use the last 2 minutes
remark-use-recent-detail = Kept in memory on this device only
remark-recent-off = Keep the last 2 minutes from now on
remark-recent-off-detail = So next time you can show what just happened
remark-recording = Recording
remark-stop = Stop
remark-capturing = Saving the last 2 minutes…
remark-attached = Recording · { $steps ->
    [one] 1 step
   *[other] { $steps } steps
} · { $duration }
remark-attached-failed = { $failed ->
    [one] 1 failed
   *[other] { $failed } failed
}
remark-remove-recording = Remove recording
remark-frames = Include screen frames
remark-frames-detail = Adds pictures of the recorded windows, so problems below the app show. Personal fields are masked.
remark-review = Review
remark-send = Send
remark-review-and-send = Review and send
remark-discard = Discard
remark-blocked-empty = Write something first
remark-blocked-nothing-picked = Pick what this is about
remark-blocked-no-destination = Choose where it goes
remark-blocked-recording = Stop the recording first
remark-blocked-capturing = Wait for the recording to finish
remark-blocked-review = Review the recording before sending
remark-blocked-no-identity = This device has no identity to share the recording as
remark-sending = Sending…

## Pick mode

remark-pick-hint = Click to pick · Shift adds · Drag an area · Arrows walk · Enter when done
remark-pick-count = { $count ->
    [one] 1 picked
   *[other] { $count } picked
}
remark-pick-done = Done
remark-pick-nothing = Nothing here can be picked
remark-pick-personal = personal
remark-role-button = Button
remark-role-toggle-button = Toggle button
remark-role-text-input = Text field
remark-role-multiline-text-input = Text area
remark-role-search-box = Search field
remark-role-combo-box = Drop-down
remark-role-link = Link
remark-role-checkbox = Checkbox
remark-role-switch = Switch
remark-role-radio-button = Option
remark-role-tab = Tab
remark-role-menu-item = Menu item
remark-role-heading = Heading
remark-role-image = Image
remark-role-list = List
remark-role-list-item = List item
remark-role-alert = Alert
remark-role-status = Status
remark-role-window = Window
remark-role-text = Text
remark-role-label = Label
remark-role-slider = Slider
remark-role-toolbar = Toolbar
remark-role-group = Group
remark-role-dialog = Dialog
remark-role-application = App

## Review

remark-review-title = Review before sending
remark-review-subtitle = This is what will be sent. Click any word to hide it.
remark-review-model-device = { $model } on this device
remark-review-model-device-detail = Sees everything, and nothing leaves this device
remark-review-model-hosted = { $model }, { $location }
remark-review-model-hosted-detail = Sees placeholders instead of personal details
remark-review-model-floor = No review model
remark-review-model-floor-detail = Titles come from action names; personal details were found on this device
remark-review-model-setup = Set up { $model }
remark-review-model-progress = { $progress }
remark-review-model-setup-action = Set up on this device
remark-review-model-failed = The review model didn't answer
remark-review-step = Step { $n }
remark-review-cut = Cut step
remark-review-keep = Keep step
remark-review-cut-state = Cut, not sent
remark-review-hidden = { $count ->
    [one] 1 hidden
   *[other] { $count } hidden
}
remark-review-failed = Failed
remark-review-refused = Refused
remark-review-verdict-logic = Replays exactly
remark-review-verdict-below = Replay differs below the app
remark-review-verdict-unsemantic = Some of it can't be replayed
remark-review-verdict-pending = Checking the replay…
remark-review-verdict-pending-detail = Replaying the session on this device against its frames
remark-review-verdict-logic-detail = Its frames match a replay on this device: a logic or UX problem
remark-review-verdict-below-detail = { $count ->
    [one] One place differs, marked on its step
   *[other] { $count } places differ, marked on their steps
}
remark-review-verdict-unavailable = Not checked against a replay
remark-review-verdict-unavailable-detail = This device can't replay the session; its frames go as recorded
remark-review-verdict-failed = Couldn't check the replay
remark-review-attach-regions = Attach differing frames
remark-review-back = Back
remark-review-shown = On screen
remark-review-included = What is included
remark-review-nothing-included = Nothing typed or shown in this step
remark-review-empty = Nothing was tried in this recording.

## After sending

remark-sent-team = Sent to the Tilo team
remark-sent-community = Posted in Tilo Community #feedback
remark-sent-person = Sent to { $name }
remark-sent-room = Posted in { $room }
remark-sent-body = Replies arrive in Chat.
remark-open-conversation = Open conversation
remark-send-failed = Couldn't send your remark

## Crash

remark-crash-title = { $app } quit unexpectedly
remark-crash-body = Tell us what happened. The last 2 minutes can come with it.
remark-crash-body-no-buffer = Tell us what happened.
remark-crash-action = Tell us what happened

## Recording pill

remark-pill-recording = Recording
remark-pill-stop = Stop recording

## Quick Settings

remark-qs = Feedback
