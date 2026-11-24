# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-chat = Chat

d-so-tilo-lang-chat-keywords = messenger, instant messaging, conversation


d-lmdoxbnn = Conversations
    .doc = The workspace's conversations in the sidebar's order: channels outside any category, the categories each with its own, and the direct messages, in the list as people.
    .noun = { $count ->
        [one] conversation
       *[other] conversations
    }

d-5kzdptyg = Channels

d-n2xgh234 = Skills

d-fe2st4jb = Category
    .doc = A named group of channels in the sidebar.
    .noun = { $count ->
        [one] category
       *[other] categories
    }

d-hbhrds5c = Name

d-hvqgmwrk = Channels
    .doc = The server's channels.

d-xr347kui = Voice channel
    .doc = A place to talk live: joining it puts the person in its call, the system's, with whoever else joined.
    .noun = { $count ->
        [one] voice channel
       *[other] voice channels
    }

d-olqjezxu = Name

d-q7llygsa = Text channel
    .doc = Messages in one stream; any message can start a thread.
    .noun = { $count ->
        [one] text channel
       *[other] text channels
    }

d-x3haugpo = Name

d-u6b35mgm = Topic

d-ksuxpopi = Messages

d-ztou5nin = Threads
    .doc = The threads its messages started, each the one a message's `thread` names.

d-kgihivmr = Reports
    .doc = The reports filed about its messages, for the workspace's moderators.

d-hk2t6bvk = Message
    .doc = A message.
    .noun = { $count ->
        [one] message
       *[other] messages
    }

d-xfrqdy3y = Body

d-4uwxpnw2 = Reply to
    .doc = The message this one answers inline, quoted above it.

d-kxekobh6 = Thread
    .doc = The side conversation branched off this message.

d-mwily5kw = Parts
    .doc = What a message says, in a message kept as parts rather than as rich text: its pieces, as it was written.

d-4zr7hti6 = Pinned
    .doc = Whether it is pinned to its channel, which search finds.

d-ofad24rc = Reactions
    .doc = Who reacted, by emoji.

d-dh6346no = Sent view
    .doc = A view the message sends, as its sender had it among their views: a reader adds it to their own views, and its author is whoever sent the message.

d-tz5yw5dt = By
    .doc = Who wrote it where it was written, as that place named them, for a message carried over from the native Chat or relayed from another service: its own history names whoever carried or relayed it.

d-4da5gcom = Written in
    .doc = Where a message carried or relayed here was written.

d-mmlcexsa = Written
    .doc = When it was written there, in milliseconds since 1970; 0 for a message written here.

d-jcgolgdf = Reacted
    .doc = The reactions it had in the native Chat, each emoji with how many reacted with it: who reacted there is not carried, since this app names people only as its own members.

d-6luxdz2x = Relayed reactions
    .doc = What people gave it where it was written, for a message relayed here from another service: each emoji, who gave it there, and the name that place shows for them.

d-n7d27huf = Attachments
    .doc = The files it carries, handed over with it: what a bridge relays from another service, and what anyone sends with a message.

d-hk2t6bvk-filter-pinned = Pinned

d-y3mwdnru = Report
    .doc = One person's report of a message to the workspace's moderators, kept by the conversation the message is in so it outlives the message: who filed it is its history's.
    .noun = { $count ->
        [one] report
       *[other] reports
    }

d-jqcabz2h = Message
    .doc = The message reported; none once it was deleted.

d-e3zqz33a = Person
    .doc = Who wrote the reported message.

d-adip7bw4 = Said
    .doc = What the message said when it was reported, read again after it was deleted.

d-bmfbgqt6 = Reason
    .doc = Why, in the reporter's words.

d-3oner4zg = Decision
    .doc = What a moderator, or the workspace's moderation agent, decided about it: empty while it is open.

d-y3mwdnru-filter-message = Message

d-y3mwdnru-filter-person = Person

d-6qpql3pg = Thread
    .doc = A side conversation branched off one message.
    .noun = { $count ->
        [one] thread
       *[other] threads
    }

d-xuvfifa5 = Name

d-ijy454g6 = Messages

d-2ki7h2z5 = Archived

d-ep7fvntj = Reports
    .doc = The reports filed about its messages, for the workspace's moderators.

d-6qpql3pg-filter-archived = Archived

d-5nkbpj6q = Direct
    .doc = A private conversation between people of the workspace, in the same message model as a channel: the person who opened it and the one they opened it with, or one person alone for their notes to self.
    .noun = { $count ->
        [one] direct
       *[other] directs
    }

d-5stv6ro7 = Name

d-wph25aa5 = People
    .doc = Who is in it, each once.

d-ycadyqh5 = Person
    .doc = The one person an earlier version kept it with, before it said who is in it: what its migration reads (`carry_direct_people`).

d-y65whtrn = Messages

d-3p73kpuw = Threads

d-doiukw5i = Reports
    .doc = The reports filed about its messages, for the workspace's moderators.

d-5nkbpj6q-filter-person = Person

d-5rctnita = Attached
    .doc = A file a message carries, as the place it was copied from keeps it: its name, what it is, how many bytes, and the address of its content.
    .noun = { $count ->
        [one] attached
       *[other] attacheds
    }

d-qsdc77jc = Name

d-h5lqkkjt = Media

d-g7nx7a7g = Size

d-wu2n7dpm = Blob
    .doc = The address its bytes read at: `tilo-blob://<hash>`.

d-5rctnita-filter-size = Size

d-3kzifrct = Relayed reaction
    .doc = A reaction someone gave a message where it was written, relayed here with the message: its emoji, the account there that gave it, and the name that place shows for them.
    .noun = { $count ->
        [one] relayed reaction
       *[other] relayed reactions
    }

d-r2zcptjt = Emoji

d-awgtnj33 = Persona
    .doc = Who gave it there, as the relay names them: the service and the account (`discord/3001`).

d-hammizbi = By
    .doc = The name that place shows for them.

d-yx5sbebu = Part
    .doc = A piece of a message kept as parts rather than as rich text: its text, or a block's source.
    .noun = { $count ->
        [one] part
       *[other] parts
    }

d-zevnbs3i = Kind
    .doc = Whether the piece is text or a block's source.

d-3hkzkqvr = Body

d-fp7zx7jn = Skill
    .doc = How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work.
    .noun = { $count ->
        [one] skill
       *[other] skills
    }
    .untitled = New skill

d-gfk6pm4a = Name
    .doc = What it is called, in a few words: what an agent names it by.

d-gnfldsvz = Work
    .doc = The kind of work it is for, in a sentence or two: when it applies.

d-bn3w5doj = How
    .doc = How that work is done here.

d-ijpa4i7d = Form
    .doc = The form the result takes; empty when it has no fixed form.

d-3wlkwi6k = Model
    .doc = The model the work is done with, as `provider/model`; empty for the person's choice for the kind of work.

d-bxx5rykt = Examples
    .doc = Results the person accepted and rejected, oldest first: how the work is done, shown by what came of it.

d-so72kbuk = Example
    .doc = A result a person accepted or rejected, kept with its skill as an example of how that work is done: more like this, or less like this.
    .noun = { $count ->
        [one] example
       *[other] examples
    }

d-fspydtu6 = Text
    .doc = The result, or the part of it that is meant.

d-2u2ngevv = Accepted
    .doc = Whether the person accepted it (more like this) or rejected it (less like this).

d-agcun6lt = Note
    .doc = What the person said about it; empty when they said nothing more.

d-3ccutu3r = At
    .doc = When it was kept where this skill came from, in milliseconds since 1970; 0 for one kept here, which the skill's history says when.

d-so72kbuk-filter-accepted = Accepted

d-so72kbuk-filter-at = At

d-msf7yzn7 = New channel
    .doc = Adds a text channel to the sidebar, under `category` when it is given and about `topic` when that is.
    .w-new-channel = new-channel

d-msf7yzn7-name = Name

d-msf7yzn7-topic = Topic

d-msf7yzn7-category = Category

d-26f7sh5k = New category
    .doc = Adds a category to the sidebar.
    .w-new-category = New category

d-26f7sh5k-name = Name

d-egiu6ag4 = New voice channel
    .doc = Adds a voice channel to the sidebar, under `category` when it is given.
    .w-voice = Voice

d-egiu6ag4-name = Name

d-egiu6ag4-category = Category

d-dra3nelb = New skill
    .doc = Starts a skill, called `name`.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-dra3nelb-name = Name

d-fvl622e2 = Correct skill
    .doc = Corrects a skill: only what is given changes, and what is left out stays as it is.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-fvl622e2-name = Name

d-fvl622e2-work = Work

d-fvl622e2-how = How

d-fvl622e2-form = Form

d-fvl622e2-model = Model

d-wmxknkkc = Forget skill
    .doc = Forgets a skill: no later turn reads it.

d-4fojczoe = Keep an example
    .doc = Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it.
    .w-an-example-says-what-the-result = An example says what the result was

d-4fojczoe-text = Text

d-4fojczoe-accepted = Accepted

d-4fojczoe-note = Note

d-aqrwkeib = Remove the example
    .doc = Takes an example out of a skill.

d-aqrwkeib-example = Example

d-tkh5jonr = Carry direct people
    .doc = The direct conversations an earlier version kept with one person alone (`person`), which every member could read: in each are that person and whoever opened it, as its history says.

d-aod7dups = Carry native chat
    .doc = The conversations the native Chat kept before this app took its id over, carried into these once: its categories and channels in the sidebar's order, each with its name and topic, and every message of a channel with what it said, whether it was pinned and the id it had there.

d-uuhhrrha = Carry native skills
    .doc = The workspace's skills the native Chat kept before this app took its id over, carried into these once: what each is called, the work it is for, how the work is done here, the form of its result and the model, under the id it had there and in the order it stood, and the examples it kept, oldest first, each with what the result was, whether the person accepted it, what they said and when it was kept.

d-uhenzjex = Report a problem
    .doc = Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens.
    .did = reported a problem
    .w-say-what-went-wrong-first = Say what went wrong first

d-uhenzjex-body = Body

d-3cetzzm3 = Message
    .doc = Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time.
    .w-is-not-in-this-workspace = { $name } is not in this workspace

d-3cetzzm3-who = Who

d-rkqdq6oi = Conversation with
    .doc = The direct conversation between the invoker and `who`, none while the two of them have none: what an agent, a script or another app reads to find it, opening and making nothing.

d-rkqdq6oi-who = Who

d-3vgu5d7q = Unread
    .doc = How many messages of a direct conversation the person asking has not read: the number its row shows beside the other person, and what an agent or a script reads of it.

# refined
d-peb7lz2j = Send
    .doc = Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one.
    .did = { $count ->
        [one] wrote a message
       *[other] wrote { $count } messages
    }

d-peb7lz2j-to = To

d-peb7lz2j-body = Body

d-peb7lz2j-reply_to = Reply to

d-peb7lz2j-attachments = Attachments

d-634q3q33 = Write to
    .doc = Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar.

d-634q3q33-who = Who

d-634q3q33-body = Body

d-qaa5kdra = Send a view
    .doc = Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views.
    .w-the-view-is-empty = The view is empty

d-qaa5kdra-to = To

d-qaa5kdra-title = Title

d-qaa5kdra-definition = Definition

d-6372fg6z = Relay
    .doc = Posts a message written on another service, as it was written there: who wrote it, as that service names them, where and when.
    .did = relayed a message
    .w-say-who-wrote-the-message = Say who wrote the message

d-6372fg6z-to = To

d-6372fg6z-body = Body

d-6372fg6z-by = By

d-6372fg6z-written_in = Written in

d-6372fg6z-written = Written

d-6372fg6z-attachments = Attachments

d-6372fg6z-reply_to = Reply to

d-bhn35nvx = Relay a reaction
    .doc = Relays a reaction someone gave a message where it was written, as that place names them: the counterpart of `relay` for a reaction.
    .did = relayed a reaction
    .w-say-which-emoji = Say which emoji
    .w-say-who-gave-the-reaction = Say who gave the reaction
    .w-say-what-they-are-called-there = Say what they are called there

d-bhn35nvx-emoji = Emoji

d-bhn35nvx-persona = Persona

d-bhn35nvx-by = By

d-bhn35nvx-given = Given

d-jozxp5qy = Delete channel
    .doc = Deletes a channel and what was said in it, from the sidebar or the category it is in.

d-jozxp5qy-from = From

d-jozxp5qy-ch = Ch

d-uukhgkjq = Delete category
    .doc = Deletes a category that holds no channel.
    .w-move-or-delete-its-channels-first = Move or delete its channels first

d-uukhgkjq-c = C

d-ggtglnmb = Move channel
    .doc = Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative).

d-ggtglnmb-ch = Ch

d-ggtglnmb-category = Category

d-ggtglnmb-at = At

d-n4l2mod2 = Move category
    .doc = Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative).

d-n4l2mod2-c = C

d-n4l2mod2-at = At

d-255qjx7d = Edit
    .doc = Changes what a message says.
    .w-only-its-author-can-edit-a = Only its author can edit a message
    .w-type-a-message-first = Type a message first

d-255qjx7d-body = Body

d-vhmhrq2t = Delete
    .doc = Deletes a message from the channel or thread it is in.

d-vhmhrq2t-to = To

d-vhmhrq2t-m = M

d-2ojqeubz = Pin
    .doc = Pins a message to its channel, or unpins it.
    .w-pin = Pin
    .w-unpin = Unpin

d-2ojqeubz-pinned = Pinned

d-np3mn65k = React
    .doc = Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.

d-np3mn65k-emoji = Emoji

d-7sjai347 = Report
    .doc = Reports a message of a conversation to the workspace's moderators, saying why when the reporter does.
    .did = reported a message

d-7sjai347-to = To

d-7sjai347-m = M

d-7sjai347-reason = Reason

d-qfzfo5jh = Decide
    .doc = Settles a report: what a moderator, or the moderation agent, decided about it.
    .w-say-what-was-decided = Say what was decided

d-qfzfo5jh-decision = Decision

d-6uysx57s = Start thread
    .doc = Starts a thread from a message of a channel or a direct conversation, named after what the message says.

d-6uysx57s-c = C

d-6uysx57s-m = M

d-6uysx57s-name = Name

d-clq6mq5o = New skill~present

d-umfqj3us = Correct skill~present

d-3e27liaz = Keep example~present

d-v6wnfixl = Report problem~present

d-x32afsaz = Message person~present

d-dgrvpxre = Send view~present

d-psjj65el = Relay~present

d-xjtinzwn = Relay reaction~present

d-gwrwo3w5 = Delete category~present

d-comllwls = Edit~present

d-c2x2a6dh = Pin~present

d-5ips5yl3 = Decide~present

d-dzzuovny = Unread
    .w-unread = Unread
    .w-show-all = Show all { $n }

d-ou6h2s2u = Skills
    .w-new-skill = New skill
    .w-no-skills-yet = No skills yet
    .w-skills-say-how-work-is-done = Skills say how work is done in this workspace, and its agents read them.

d-2dkm5gja = Skill
    .w-what-it-is-called = What it is called
    .w-when-it-applies = When it applies
    .w-the-kind-of-work-this-is = The kind of work this is for, in a sentence or two
    .w-how-it-is-done-here = How it is done here
    .w-what-to-do-step-by-step = What to do, step by step where that helps
    .w-the-form-of-the-result = The form of the result
    .w-empty-when-it-has-no-fixed = Empty when it has no fixed form
    .w-the-model = The model
    .w-provider-model-empty-for-the-person = provider/model, empty for the person's choice
    .w-examples = Examples

d-gnhwq4qd = Example row
    .w-accepted-more-like-this = Accepted: more like this
    .w-rejected-less-like-this = Rejected: less like this
    .w-remove = Remove

d-5rkfnkg4 = Sidebar
    .w-channels = Channels
    .w-no-channels-yet = No channels yet
    .w-channels-are-where-your-workspace-talks = Channels are where your workspace talks, each one a topic.
    .w-notes-to-self = Notes to self
    .w-direct-messages = Direct messages
    .w-workspace = Workspace
    .w-workspace-skills = Workspace skills
    .w-no-channel-open = No channel open

d-ymtess4x = Channel
    .w-message = Message #{ $name }
    .w-send = Send
    .w-wrote-this-in-the-native-chat = { $by } wrote this in the native Chat
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit
    .w-running = Running…
    .w-working = Working…
    .w-waiting-for-your-answer = Waiting for your answer
    .w-failed = Failed
    .w-stopped = Stopped
    .w-done = Done
    .w-allow = Allow
    .w-always = Always
    .w-deny = Deny

d-jbezqvlc = Direct
    .w-message = Message { $name }
    .w-send = Send
    .w-wrote-this-in-the-native-chat = { $by } wrote this in the native Chat
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit
    .w-running = Running…
    .w-working = Working…
    .w-waiting-for-your-answer = Waiting for your answer
    .w-failed = Failed
    .w-stopped = Stopped
    .w-done = Done
    .w-allow = Allow
    .w-always = Always
    .w-deny = Deny

d-q45utnmh = Channel summary

d-qnkkcusk = Voice

d-lkjppscz = Voice summary

d-enkldjck = Direct summary

d-oqg63mfd = Category summary

d-hrbsxhor = Skill row

d-enkgjyn7 = Thread
    .w-message = Message { $name }
    .w-send = Send
    .w-wrote-this-in-the-native-chat = { $by } wrote this in the native Chat
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit
    .w-running = Running…
    .w-working = Working…
    .w-waiting-for-your-answer = Waiting for your answer
    .w-failed = Failed
    .w-stopped = Stopped
    .w-done = Done
    .w-allow = Allow
    .w-always = Always
    .w-deny = Deny

d-rydgecpc = Typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-tx3zjawu = Direct typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-dcprca6g = Thread typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-h327uiql = Turn
    .w-working = Working…
    .w-waiting-for-your-answer = Waiting for your answer
    .w-failed = Failed
    .w-stopped = Stopped
    .w-done = Done
    .w-allow = Allow
    .w-always = Always
    .w-deny = Deny

d-hjw5xd6b = Said
    .w-a-view-to-add-to-your = A view, to add to your own views
    .w-edited = edited
