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

d-uwopf5u7 = Reply
    .doc = An agent's answer to an ask, as it writes it.
    .noun = { $count ->
        [one] reply
       *[other] replies
    }

d-gjd462ep = Ask

d-oka345cb = Body
    .doc = The answer, growing while the agent writes.

d-hzlmnzzw = State

d-tnkzsmes = Attempt
    .doc = Which time it was asked: the first reply is 1.

d-e36wbuu7 = Steps
    .doc = What the agent called on the way, in order.

d-eyh6zmbf = Superseded
    .doc = Whether another device's answer to the same ask stands instead: two devices offline at once may both have answered.

d-uwopf5u7-filter-ask = Ask

d-uwopf5u7-filter-state = State

d-uwopf5u7-filter-attempt = Attempt

d-uwopf5u7-filter-superseded = Superseded

d-7nochn55 = Agents
    .doc = Every ask and reply of the workspace.
    .noun = { $count ->
        [one] agent
       *[other] agents
    }

d-mzzoxssv = Asks

d-353i7jsi = Replies

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

d-5zilxes5 = Stop
    .doc = Stops a turn that is working; what the agent did until then stays.
    .w-nothing-is-working-on-it = Nothing is working on it

d-zl7upzy7 = Answer
    .doc = The agent starts its reply to `a`, the ask's next attempt.
    .did = answered
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-nobody-waits-for-an-answer = Nobody waits for an answer

d-s6gerkjs = Say
    .doc = The agent writes `more` at the end of its answer.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-s6gerkjs-more = More

d-6s74tahg = Step
    .doc = The agent called `called` on the way, and it answered `answered`.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-the-answer-is-finished = The answer is finished

d-6s74tahg-called = Called

d-6s74tahg-answered = Answered

d-6s74tahg-failed = Failed

d-6koj42w5 = Finish
    .doc = The agent's turn ended as `state`: done, failed, or stopped when a person stopped it.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer
    .w-a-turn-ends-done-failed-or = A turn ends done, failed or stopped

d-6koj42w5-state = State

d-5n4bbzji = Supersede
    .doc = Another device's answer to the same ask stands instead of `r`.
    .w-only-the-agent-asked-may-answer = Only the agent asked may answer

d-koyxjtxv = Withdraw~present

d-rxmz2d33 = Reopen~present

d-mzbpkahz = Stop~present

d-o6oeh6vp = Answer~present

d-hdzxkm76 = Say~present

d-k36fktxl = Step~present

d-3ebfgkt2 = Finish~present

d-wsxdear2 = Supersede~present

d-zfzqq5xy = Asked
    .w-nobody-asked-an-agent-yet = Nobody asked an agent yet

d-bigklctq = Ask row

d-7fu4ujho = Reply

d-sglhetw2 = Replies

d-rwsixj7g = Ask page
    .w-the-agent-may-not-read-what = The agent may not read what it was asked about.
