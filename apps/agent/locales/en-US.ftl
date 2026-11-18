## What the agent says to the person whose task it runs.
##
## A run's reply, its progress and its failures are the agent's own words, so
## they are messages here, in the language the person chose for their desktop.
## What a model answers is not here: it arrives in whatever language it was
## asked in.

agent-plan-model = Planning with { $model } from { $provider }.
agent-plan-no-model = I can't work on this yet: no model is set up. Choose one in Settings, under Agent models.
agent-plan-models-off = Models are turned off for this agent, so nothing can plan this run. Start it again with a model turned on.

agent-step-searching = Searching the web for “{ $what }”…
agent-step-reading = Reading { $what }…
agent-step-editing = Editing { $what }…
agent-step-exploring = Looking at what { $what } can do…
agent-step-on = { $action }: { $subject }
agent-step-edited = Edited { $what }
agent-step-searched = Searched the web for “{ $what }”
agent-step-read = Read { $what }

agent-cancel-thinking = Cancelled while thinking

agent-fail-spend-limit = { $agent } has used today's { $spent }; ask its keeper to raise it

# What a failed model call leaves in the reply a person reads: one sentence
# of what happened, in their own terms, and where there is a setting to look
# at, which one. The transport's error, the partial answer, the provider's
# exposed reasoning and the recovery instruction the agent follows are the
# run's working resource, never this copy.
agent-failure-unavailable = The model's service is unavailable right now.
agent-failure-offline = No model while offline: your model is asked on your workspace's server, which this computer can't reach right now.
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
