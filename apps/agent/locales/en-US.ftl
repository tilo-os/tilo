## What the agent says to the person whose task it runs.
##
## A run's reply, its progress and its failures are the agent's own words, so
## they are messages here, in the language the person chose for their desktop.
## What a model answers is not here: it arrives in whatever language it was
## asked in.

## How a run's steps were chosen

agent-plan-model = Planning with { $model } from { $provider }.
agent-plan-words-asked = This run is planned without a model, so its steps are matched to the actions this workspace's apps publish.
agent-plan-words-no-model = No model is set up to plan this run, so its steps are matched to the actions this workspace's apps publish. Choose a model in Settings, under Agent models, to have one plan instead.
agent-plan-no-action = No published action carries the words of the request.
agent-plan-unmatched-asked = { $why } Name the app to work in, or say what to do in it.
agent-plan-no-model = I can't work on this yet: no model is set up. Choose one in Settings, under Agent models.
agent-plan-models-off = Models are turned off for this agent, so nothing can plan this run. Start it again with a model turned on.

## How a run ended

agent-done-model = { $actions ->
        [one] Completed one action.
       *[other] Completed { $actions } actions.
    }
agent-done-words-asked = { $actions ->
        [one] Done: one action matched to the request's words.
       *[other] Done: { $actions } actions matched to the request's words.
    }
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

agent-run-reading-actions = Reading the actions this workspace's apps publish
agent-run-step = Running { $index } of { $total }: { $action }
agent-model-turn-completed = Model turn completed
agent-step-searching = Searching the web for “{ $what }”…
agent-step-reading = Reading { $what }…
agent-step-editing = Editing { $what }…
agent-step-exploring = Looking at what { $what } can do…
agent-step-program = a program

## A run that stopped

agent-run-needs-decision = Needs your decision before continuing
agent-run-directions-require-review = Directions need model planning or your review; none were acknowledged
agent-run-cancelled-before-planning = Cancelled before planning
agent-run-cancelled-before-action = Cancelled before the next action
agent-run-cancelled-before-step = Cancelled before the next step
agent-run-cancelled-in-action = Cancelled while an action was running
agent-cancel-task-center = Cancelled from Task Center
agent-cancel-thinking = Cancelled while thinking
agent-cancel-superseded = Superseded by new directions

## A run that failed, and what the person can do about it

agent-fail-no-terminal-state = The task's action hit its deadline without ending.
agent-fail-local-only = Tasks can't run an action that only works on this device.
agent-fail-arguments = The planned action's arguments don't fit: { $error }
agent-fail-turns = The task used all { $turns } model turns of its budget before it finished ({ $actions } action(s) completed). Raise its turns and run it again.
agent-fail-time = The task ran out of time before it finished.
agent-fail-budget = The task's time budget ran out.
agent-fail-model-call = The model call failed: { $error }. Check the model in Settings, under Agent models, then run this again.
agent-fail-model-answer = The model's answer couldn't be used: { $error }. Run this again.
agent-fail-turn-not-finished = The model turn did not finish in time
agent-fail-native-outcome = The action's outcome is still unknown; check the history before trying again.
agent-fail-direction-pages = More than one page of new directions needs your review; none were acknowledged.
