# The shell's own strings: the remark surface (say something about any
# part of any app), pick mode, recording and review before sending.

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
remark-review-verdict-logic = Replays exactly: a logic or UX problem
remark-review-verdict-below = Frames differ from the replay: likely below the app
remark-review-verdict-unsemantic = Part of the screen has no structure to replay
remark-review-verdict-pending = Checking the replay…
remark-review-attach-regions = Attach differing frames
remark-review-back = Back
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
