# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-agent = Agents

d-zbfroywd = Ask
    .doc = Someone asked an agent something about a resource: by mentioning it in a chat message, a note or a task, by writing to it directly, or from the canvas.
    .noun = { $count ->
        [one] ask
       *[other] asks
    }

d-mcsi6vp5 = Agent
    .doc = The agent asked.

d-7j5pok54 = Said
    .doc = What was asked, as it read when asked: a later edit of the text asks nothing new.

d-hw6dxbyx = Mention
    .doc = The mention that asked: the agent's reference as the text marks it; empty for an ask made outright.

d-wtm364ne = State

d-zbfroywd-filter-agent = Agent

d-zbfroywd-filter-state = State

d-bryguujp = Step
    .doc = One step of a turn: an action the agent called, with what it answered.
    .noun = { $count ->
        [one] step
       *[other] steps
    }

d-ippal7k2 = Called
    .doc = The action it called, by its address.

d-cyu3le2j = Answered
    .doc = What it answered, as the agent read it.

d-cma7vgpx = Failed
    .doc = Whether it failed.

d-bryguujp-filter-failed = Failed

d-xhzlueny = Sent view
    .doc = A view an agent answered with, as a Chat message sends one: what it is called, and the view as tilolang source, drawn live for each reader as they read.
    .noun = { $count ->
        [one] sent view
       *[other] sent views
    }

d-gdhl4pzx = Title

d-ytw765gn = Definition

d-bdijpmdi = Shown
    .doc = A thing an agent shows in its reply: a card of it, live, which opens it.
    .noun = { $count ->
        [one] shown
       *[other] showns
    }

d-mx5wcfl2 = Card
    .doc = The card: one paragraph projecting the thing.

d-uwopf5u7 = Reply
    .doc = An agent's answer to an ask, as it writes it.
    .noun = { $count ->
        [one] reply
       *[other] replies
    }

d-gjd462ep = Ask

d-oka345cb = Body
    .doc = The answer, growing while the agent writes.

d-czzg7mhh = Thinking
    .doc = What the agent's model thought on the way, where its provider shows it: growing while it thinks, and never part of the answer.

d-hzlmnzzw = State

d-tnkzsmes = Attempt
    .doc = Which time it was asked: the first reply is 1.

d-e36wbuu7 = Steps
    .doc = What the agent called on the way, in order.

d-bnlsdtf6 = Views
    .doc = The views it answered with, in order.

d-mwwrjl52 = Shown
    .doc = The things it showed, in order.

d-eyh6zmbf = Superseded
    .doc = Whether another device's answer to the same ask stands instead: two devices offline at once may both have answered.

d-uwopf5u7-filter-ask = Ask

d-uwopf5u7-filter-state = State

d-uwopf5u7-filter-attempt = Attempt

d-uwopf5u7-filter-superseded = Superseded

d-kprk52dn = Summary
    .doc = What an agent keeps of what a conversation said before the part a turn is shown, so a conversation longer than a turn can keep what it said earlier.
    .noun = { $count ->
        [one] summary
       *[other] summaries
    }

d-fbk2rxka = Agent
    .doc = The agent that keeps it.

d-wl4g23ke = Text
    .doc = What the conversation said before, in the agent's words.

d-hefqltbc = Through
    .doc = The item it reaches: the conversation's message the agent last took in.

d-cs27n4h5 = Through at
    .doc = When that item was written, ISO-8601 UTC.

d-kprk52dn-filter-agent = Agent

d-7nochn55 = Agents
    .doc = Every ask and reply of the workspace, and what its agents keep of its long conversations.
    .noun = { $count ->
        [one] agent
       *[other] agents
    }

d-mzzoxssv = Asks

d-353i7jsi = Replies

d-srbtof5j = Summaries

d-zofobn2n = Ask state
    .doc = Where an ask stands.

d-lkb24n4s = Asked, and no device has taken the turn yet
    .doc = Asked, and no device has taken the turn yet.

d-a2isbfl6 = A device took the turn and the agent is working on it
    .doc = A device took the turn and the agent is working on it.

d-h6ysttlf = The agent answered
    .doc = The agent answered.

d-jjhgercj = The turn ended without an answer; its reply says why
    .doc = The turn ended without an answer; its reply says why.

d-4rnpzqhx = A person stopped the turn while it worked
    .doc = A person stopped the turn while it worked.

d-rcdvp2yi = The asker took it back, or removed the mention that asked, before a device took it
    .doc = The asker took it back, or removed the mention that asked, before a device took it.

d-ovntytjy = The agent may not read what it was asked about, so nothing of it was copied here
    .doc = The agent may not read what it was asked about, so nothing of it was copied here.

d-szvp4mak = Ask
    .doc = Asks `agent` about the resource the ask is made from (`.made_from(r)`).
    .did = asked

d-szvp4mak-agent = Agent

d-szvp4mak-said = Said

d-szvp4mak-mention = Mention

d-q24an2fk = Withdraw
    .doc = Takes an ask back before a device took its turn.
    .w-only-who-asked-can-withdraw-it = Only who asked can withdraw it
    .w-the-agent-is-already-on-it = The agent is already on it

d-so4an2tr = Ask again
    .doc = Asks again what was withdrawn: the mention that made it is back, as redo puts it back.
    .w-only-who-asked-can-ask-it = Only who asked can ask it again
    .w-it-was-not-withdrawn = It was not withdrawn

d-xamg74g2 = Retry
    .doc = Asks again for an answer that failed: the same ask, its next attempt, so the reply written now stands beside the one that failed.
    .w-only-who-asked-can-ask-it = Only who asked can ask it again
    .w-it-did-not-fail = It did not fail

d-5zilxes5 = Stop
    .doc = Stops a turn that is working; what the agent did until then stays.
    .w-nothing-is-working-on-it = Nothing is working on it

d-zl7upzy7 = Answer
    .doc = The agent starts its reply to `a`, the ask's next attempt: the one after its newest reply, so a reply that failed and was asked again stands beside it.
    .did = answered
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-nobody-waits-for-an-answer = Nobody waits for an answer

d-s6gerkjs = Say
    .doc = The agent writes `more` at the end of its answer.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-s6gerkjs-more = More

d-jjuslny3 = Think
    .doc = The agent's model thought `more` on the way, as its provider shows it: at the end of the reply's thinking, beside its answer.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-jjuslny3-more = More

d-6s74tahg = Step
    .doc = The agent called `called` on the way, and it answered `answered`.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-6s74tahg-called = Called

d-6s74tahg-answered = Answered

d-6s74tahg-failed = Failed

d-jriyp5ei = Show view
    .doc = The agent answers with a view: `definition`, called `title`.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished
    .w-the-view-is-empty = The view is empty

d-jriyp5ei-title = Title

d-jriyp5ei-definition = Definition

d-lym5tesj = Show
    .doc = The agent shows the thing `card` projects in its answer, live.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-lym5tesj-card = Card

d-6koj42w5 = Finish
    .doc = The agent's turn ended as `state`: done, failed, or stopped when a person stopped it.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-a-turn-ends-done-failed-or = A turn ends done, failed or stopped

d-6koj42w5-state = State

d-5n4bbzji = Supersede
    .doc = Another device's answer to the same ask stands instead of `r`.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer

d-q7wtfy6u = Keep a summary
    .doc = The agent keeps what the conversation this is made from said before the part its turn was shown: `text` says it in the agent's words, reaching `through`, the item it last took in.
    .w-only-the-agent-may-keep-its = Only the agent may keep its summary

d-q7wtfy6u-agent = Agent

d-q7wtfy6u-text = Text

d-q7wtfy6u-through = Through

d-q7wtfy6u-through_at = Through at

d-koyxjtxv = Withdraw~present

d-rxmz2d33 = Reopen~present

d-za2dxf3d = Retry~present

d-mzbpkahz = Stop~present

d-o6oeh6vp = Answer~present

d-hdzxkm76 = Say~present

d-645ej5zc = Think~present

d-k36fktxl = Step~present

d-zasvncsi = Show view~present

d-64ypkzcb = Show~present

d-3ebfgkt2 = Finish~present

d-wsxdear2 = Supersede~present

d-hcwspess = Keep summary~present

d-zfzqq5xy = Asked
    .w-nobody-asked-an-agent-yet = Nobody asked an agent yet

d-bigklctq = Ask row

d-7fu4ujho = Reply
    .w-thinking = Thinking
    .w-thought = Thought

d-sglhetw2 = Replies

d-rwsixj7g = Ask page
    .w-the-agent-may-not-read-what = The agent may not read what it was asked about.
