# Chat — user-facing strings.

# Marker beside a participant in the `@` picker that is an agent rather than a
# person, so somebody can tell at a glance that they are addressing software.
chat-mention-agent-tag = agent

# Header above the list of participants offered after typing `@`.
chat-mention-picker-title = Mention

# Shown in the composer's placeholder line.
chat-composer-mention-hint = Type @ to mention someone

# --- An agent's record of what it did with a message addressed to it -------
#
# Written onto the agent's own threaded reply and updated in place, so these
# lines are read beneath the message that summoned it as well as on the reply
# itself. Every one of them is past or present tense about something that has
# already happened; none of them promises anything.

# The summons was taken and nothing is decided yet. { $at } is when.
chat-agent-took = { $agent } is reading this · { $at }

# The request names something owned by an app whose state has not arrived.
chat-agent-waiting = { $agent } is waiting for { $app } · { $at }

# What the agent decided the request was, in the app's own words.
chat-agent-working = { $agent } is working · { $did } · { $at }

# The same, when the request named what it was about.
chat-agent-working-about = { $agent } is working · { $did } on { $about } · { $at }

# The operation was applied.
chat-agent-done = { $agent } did { $did } · { $at }

# The agent will not act. Its own words follow on the next line.
chat-agent-stopped = { $agent } stopped · { $at }

# An unfinished record left behind by an agent that is no longer in the room.
# Deliberately not present tense: nothing is happening, and the last thing
# anybody can honestly say is when it stopped.
chat-agent-gone = { $agent } took this at { $at } and is no longer here

# Beside an agent in the members list, when it has work in flight.
chat-roster-agent-working = Working
