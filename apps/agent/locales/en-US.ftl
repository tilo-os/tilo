## What the agent says to the person whose task it runs.
##
## A run's reply, its progress and its failures are the agent's own words, so
## they are messages here, in the language the person chose for their desktop.
## What a model answers is not here: it arrives in whatever language it was
## asked in.

## How a run's steps were chosen

agent-plan-model = Planning with { $model } from { $provider }.
agent-plan-words-no-model = No model is set up to plan this run, so its steps are matched to the actions this workspace's apps publish. Choose a model in Settings, under Agent models, to have one plan instead.
agent-plan-no-action = No published action carries the words of the request.
agent-plan-no-model = I can't work on this yet: no model is set up. Choose one in Settings, under Agent models.
agent-plan-models-off = Models are turned off for this agent, so nothing can plan this run. Start it again with a model turned on.
agent-plan-model-failed = The model didn't answer, so this run couldn't be planned. Try again in a moment.

## How a run ended

agent-done-words-no-model = { $actions ->
        [one] Done: one action matched to the request's words, without a model.
       *[other] Done: { $actions } actions matched to the request's words, without a model.
    }

## Requests the words could not be matched to

agent-match-nothing = I can't do that here.
agent-match-several = That could mean more than one thing. Which one?
agent-match-one-of = That could mean { $actions }. Which one?
agent-match-or = { $a } or { $b }
agent-match-needs-text = “{ $label }” needs the words to work with, in quotes.

## A run's progress

agent-session-reading-actions = Reading the actions this workspace's apps publish
agent-session-step = Running { $index } of { $total }: { $action }
agent-model-turn-completed = Model turn completed
agent-step-searching = Searching the web for “{ $what }”…
agent-step-reading = Reading { $what }…
agent-step-editing = Editing { $what }…
agent-step-exploring = Looking at what { $what } can do…
agent-step-program = Looked things up
agent-step-looked-up = Looked up { $what }
agent-step-on = { $action }: { $subject }
agent-step-edited = Edited { $what }
agent-step-searched = Searched the web for “{ $what }”
agent-step-read = Read { $what }

## A run that stopped

agent-session-needs-decision = Needs your decision before continuing
agent-session-cancelled-before-planning = Cancelled before planning
agent-session-cancelled-before-action = Cancelled before the next action
agent-session-cancelled-before-step = Cancelled before the next step
agent-session-cancelled-in-action = Cancelled while an action was running
agent-cancel-task-center = Cancelled from Task Center
agent-cancel-thinking = Cancelled while thinking

## A run that failed, and what the person can do about it

agent-fail-no-terminal-state = The task's action hit its deadline without ending.
agent-fail-local-only = Tasks can't run an action that only works on this device.
agent-fail-arguments = The planned action's arguments don't fit: { $error }
agent-fail-turn-not-finished = The model turn did not finish in time
agent-fail-native-outcome = The action's outcome is still unknown; check the history before trying again.
agent-fail-spend-limit = { $agent } has used today's { $spent }; ask its keeper to raise it

# What a failed model call leaves in the reply a person reads: one sentence
# of what happened, in their own terms, and where there is a setting to look
# at, which one. The transport's error, the partial answer, the provider's
# exposed reasoning and the recovery instruction the agent follows are the
# run's working record, never this copy.
agent-failure-unavailable = The model's service is unavailable right now.
agent-failure-no-provider = No model is set up. Choose one in Settings, under Agent models.
agent-failure-authentication = The model's provider did not accept its credential. Check it in Settings.
agent-failure-permission-denied = The model's provider refused this request.
agent-failure-invalid-request = The model could not take this request. Check the model in Settings.
agent-failure-conflict = The model settings changed while this ran. Check the model in Settings.
agent-failure-invalid-response = The model's answer could not be used.
agent-failure-unsupported = The model cannot do this. Choose another model in Settings, under Agent models.
agent-failure-rate-limited = The model is busy right now. Try again in a moment.
agent-failure-credits-exhausted = The model's provider is out of credit.
agent-failure-context-limit = The conversation is longer than the model can take. Choose a model with more room.
agent-failure-provider = The model's provider failed. Try again.
agent-failure-timed-out = The model didn't answer in time.
agent-failure-cancelled = The model call was stopped.
agent-failure-truncated = The model's answer was cut off.
agent-failure-refused = The model declined this request.
agent-failure-transport = The model could not be reached. Try again.
agent-failure-no-answer = The agent finished without doing anything or saying what it did.
agent-failure-empty-answer = The agent finished without an answer. Try asking again.
