# tilolang messages: generated from the package's source. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-chat = Chat

d-so-tilo-chat-keywords = messenger, instant messaging, conversation

d-lmdoxbnn = Conversations
    .doc = The workspace's conversations in the sidebar's order: channels outside any category, the categories each with its own, and the direct messages, in the list as people.
    .noun = { $count ->
        [one] conversation
       *[other] conversations
    }

d-5kzdptyg = Channels

d-n2xgh234 = Skills

d-64vv225r = Drafts
    .doc = What each person is writing in a conversation and has not sent.

d-fe2st4jb = Category
    .doc = A named group of channels in the sidebar.
    .noun = { $count ->
        [one] category
       *[other] categories
    }

d-hbhrds5c = Name

d-hvqgmwrk = Channels
    .doc = The server's channels.

d-5pfbdqqj = Service
    .doc = The outside service this conversation is carried to, as its people read it ("Discord"); empty for one that stays here.

d-vvs4ecqz = Outside id
    .doc = Its id there, when a bridge carries it: the channel, category or conversation there.

d-xr347kui = Voice channel
    .doc = A place to talk live: joining it puts the person in its call, the system's, with whoever else joined.
    .noun = { $count ->
        [one] voice channel
       *[other] voice channels
    }

d-olqjezxu = Name

d-enzjdbyp = Service
    .doc = The outside service this conversation is carried to, as its people read it ("Discord"); empty for one that stays here.

d-nvzanb5n = Outside id
    .doc = Its id there, when a bridge carries it: the channel, category or conversation there.

d-q7llygsa = Text channel
    .doc = Messages in one stream; any message can start a thread.
    .noun = { $count ->
        [one] text channel
       *[other] text channels
    }

d-x3haugpo = Name

d-u6b35mgm = Topic

d-fld77g72 = Summary
    .doc = What has been said here so far, in a few paragraphs, as an agent last wrote it down: what a turn reads in place of the messages it summarizes.

d-ksuxpopi = Messages

d-ztou5nin = Threads
    .doc = The threads its messages started, each the one a message's `thread` names.

d-kgihivmr = Reports
    .doc = The reports filed about its messages, for the workspace's moderators.

d-byf6kyop = Archived
    .doc = Whether the channel is set aside: it keeps every message and its place in the sidebar is gone until it is restored.

d-eqr5qd3v = Service
    .doc = The outside service this conversation is carried to, as its people read it ("Discord"); empty for one that stays here.

d-elvv3ebj = Outside id
    .doc = Its id there, when a bridge carries it: the channel, category or conversation there.

d-q7llygsa-filter-archived = Archived

d-3ngvkwox = Outside author
    .doc = Who wrote a message somewhere else, as that place shows them: a bridge's generic outside author, never one service's shape.
    .noun = { $count ->
        [one] outside author
       *[other] outside authors
    }

d-5fxndhc2 = Name
    .doc = The name they go by there.

d-zkszdahn = Avatar
    .doc = The address of their picture there, empty for none.

d-yycajap4 = Service
    .doc = What the place they wrote from is called, as its people read it ("Discord").

d-p3wfphnr = Outside emoji
    .doc = A picture a message from outside carries in its words: a custom emoji of that service.
    .noun = { $count ->
        [one] outside emoji
       *[other] outside emojis
    }

d-ldtylgfu = Name

d-jjutyqgx = Picture
    .doc = The address of its picture.

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

d-hj4axvbu = Turn
    .doc = The agent's turn that wrote it: the id of the `#answer` invocation it answers in, which Stop cancels and whose steps, questions and cost are the turn's.

d-ofad24rc = Reactions
    .doc = Who reacted, by emoji.

d-dh6346no = Sent view
    .doc = A view the message sends, as its sender had it among their views: a reader adds it to their own views, and its author is whoever sent the message.

d-wqxsenb3 = Outside
    .doc = Who wrote it, when it was written somewhere else: as that place shows them, with the picture they wear there and the service's name.

d-bbu6225x = Outside emoji
    .doc = The pictures its words carry from where it was written: that service's custom emoji, each drawn by the address it gives.

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

d-cv664irm = Summary
    .doc = What has been said here so far, in a few paragraphs, as an agent last wrote it down: what a turn reads in place of the messages it summarizes.

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
    .doc = Who is in it.

d-ycadyqh5 = Person
    .doc = The one person an earlier version kept it with, before it said who is in it: what its migration reads (`carry_direct_people`).

d-s7ol3kl4 = Summary
    .doc = What has been said here so far, in a few paragraphs, as an agent last wrote it down: what a turn reads in place of the messages it summarizes.

d-y65whtrn = Messages

d-3p73kpuw = Threads

d-doiukw5i = Reports
    .doc = The reports filed about its messages, for the workspace's moderators.

d-a7u4jddr = Service
    .doc = The outside service this conversation is carried to, as its people read it ("Discord"); empty for one that stays here.

d-f4cfgfpu = Outside id
    .doc = Its id there, when a bridge carries it: the channel, category or conversation there.

d-5nkbpj6q-filter-people = People

d-5nkbpj6q-filter-person = Person

d-lhl5rxcq = Draft
    .doc = What a person is writing in a conversation and has not sent.
    .noun = { $count ->
        [one] draft
       *[other] drafts
    }

d-5shwuvrs = Conversation
    .doc = The conversation it is written in: a channel, a thread or a direct conversation.

d-rxmavdj2 = Body

d-o5vmbr2v = Files
    .doc = The files it will carry, in the order they were chosen: the system's chooser adds them, the composer draws them, and the send that makes the message takes them onto it.

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

d-ldhzcfxm = Bridge
    .doc = How one kind of work is done in this workspace: what a person writes down once, and what an agent reads before it starts that work.
    .w-say-which-service-carries-the-conversation = Say which service carries the conversation

d-ldhzcfxm-c = C

d-ldhzcfxm-service = Service

d-ldhzcfxm-outside_id = Outside id

d-yczlubh7 = Unbridge
    .doc = Keeps a conversation here: no service carries it any more.

d-yczlubh7-c = C

d-4scklxqs = New channel
    .doc = Adds a text channel to the sidebar, under `category` when it is given and about `topic` when that is.
    .w-new-channel = new-channel

d-4scklxqs-name = Name

d-4scklxqs-topic = Topic

d-4scklxqs-category = Category

d-cq7rcfwg = New category
    .doc = Adds a category to the sidebar.
    .w-new-category = New category

d-cq7rcfwg-name = Name

d-f4qj2rwa = New voice channel
    .doc = Adds a voice channel to the sidebar, under `category` when it is given.
    .w-voice = Voice

d-f4qj2rwa-name = Name

d-f4qj2rwa-category = Category

d-daluhfsk = New skill
    .doc = Starts a skill, called `name`.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-daluhfsk-name = Name

d-6jgrwebi = Correct skill
    .doc = Corrects a skill: only what is given changes, and what is left out stays as it is.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-6jgrwebi-name = Name

d-6jgrwebi-work = Work

d-6jgrwebi-how = How

d-6jgrwebi-form = Form

d-6jgrwebi-model = Model

d-hfw4gkd3 = Forget skill
    .doc = Forgets a skill: no later turn reads it.

d-gmpqlube = Keep an example
    .doc = Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it.
    .w-an-example-says-what-the-result = An example says what the result was

d-gmpqlube-text = Text

d-gmpqlube-accepted = Accepted

d-gmpqlube-note = Note

d-44nvfzzq = Remove the example
    .doc = Takes an example out of a skill.

d-44nvfzzq-example = Example

d-fom6d6su = Carry direct people
    .doc = The direct conversations an earlier version kept with one person alone (`person`), which every member could read: in each are that person and whoever opened it, as its history says.

d-bi2i7p2d = Carry native chat
    .doc = The conversations the native Chat kept before this app took its id over, carried into these once: its categories and channels in the sidebar's order, each with its name and topic, and every message of a channel with what it said, whether it was pinned and the id it had there.

d-iyehwolt = Carry native skills
    .doc = The workspace's skills the native Chat kept before this app took its id over, carried into these once: what each is called, the work it is for, how the work is done here, the form of its result and the model, under the id it had there and in the order it stood, and the examples it kept, oldest first, each with what the result was, whether the person accepted it, what they said and when it was kept.

d-yqispavu = Report a problem
    .doc = Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens.
    .did = reported a problem
    .w-say-what-went-wrong-first = Say what went wrong first

d-yqispavu-body = Body

d-4cfw6ty2 = Message
    .doc = Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time.
    .w-is-not-in-this-workspace = { $name } is not in this workspace

d-4cfw6ty2-who = Who

d-aaqqwyl3 = Conversation with
    .doc = The direct conversation between the invoker and `who`, none while the two of them have none: what an agent, a script or another app reads to find it, opening and making nothing.

d-aaqqwyl3-who = Who

d-uxndjwde = Unread
    .doc = How many messages of a direct conversation the person asking has not read: the number its row shows beside the other person, and what an agent or a script reads of it.

d-q5e45hla = Open channel
    .doc = Opening a channel makes the draft its person writes in, when they have none: a press away and back finds what they typed, on every device of theirs, and nobody else reads it.

d-yfjnxpbn = Open direct
    .doc = {""}[`open_channel`] for a direct conversation.

d-hs67kfnp = Open thread
    .doc = {""}[`open_channel`] for a thread.

d-peb7lz2j = Send
    .doc = Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one.
    .did = { $count ->
        [one] wrote a message
       *[other] wrote { $count } messages
    }
    .w-type-a-message-first = Type a message first

d-peb7lz2j-to = To

d-peb7lz2j-body = Body

d-peb7lz2j-reply_to = Reply to

d-peb7lz2j-attachments = Attachments

d-peb7lz2j-turn = Turn

d-peb7lz2j-draft = Draft

d-obp6lcdr = Attach files
    .doc = Adds the files a person chose to the message they are writing: the system's chooser runs this with what was chosen, and the draft holds them until it is sent.
    .did = attached files

d-obp6lcdr-files = Files

d-qkcjgxr6 = Remove the file
    .doc = Takes a file off the message a person is writing, before it is sent.

d-qkcjgxr6-file = File

d-mfxqzosk = Open attachment
    .doc = Opens the file a message carries in the app that opens its kind: the file reaches that app as the same content both read, never as a path neither has.

d-kn2iqm4w = Write to
    .doc = Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar.

d-kn2iqm4w-who = Who

d-kn2iqm4w-body = Body

d-v3slhm65 = Summarize
    .doc = Writes down what has been said in a conversation so far, replacing what was written before: what an agent's next turn there reads in place of the messages it covers.

d-v3slhm65-to = To

d-v3slhm65-summary = Summary

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

d-6372fg6z-avatar = Avatar

d-6372fg6z-service = Service

d-6372fg6z-outside_emoji = Outside emoji

d-shvpmvqy = Relay a reaction
    .doc = Relays a reaction someone gave a message where it was written, as that place names them: the counterpart of `relay` for a reaction.
    .did = relayed a reaction
    .w-say-which-emoji = Say which emoji
    .w-say-who-gave-the-reaction = Say who gave the reaction
    .w-say-what-they-are-called-there = Say what they are called there

d-shvpmvqy-emoji = Emoji

d-shvpmvqy-persona = Persona

d-shvpmvqy-by = By

d-shvpmvqy-given = Given

d-jozxp5qy = Delete channel
    .doc = Deletes a channel and what was said in it, from the sidebar or the category it is in.
    .w-only-the-workspace-s-moderators-delete = Only the workspace's moderators delete a channel

d-jozxp5qy-from = From

d-jozxp5qy-ch = Ch

d-e3rwn5pb = Delete category
    .doc = Deletes a category that holds no channel.
    .w-only-the-workspace-s-moderators-delete = Only the workspace's moderators delete a category
    .w-move-or-delete-its-channels-first = Move or delete its channels first

d-e3rwn5pb-c = C

d-4kkr24cm = Move channel
    .doc = Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative).

d-4kkr24cm-ch = Ch

d-4kkr24cm-category = Category

d-4kkr24cm-at = At

d-6rigb6x4 = Move category
    .doc = Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative).

d-6rigb6x4-c = C

d-6rigb6x4-at = At

d-k5m36ngd = Rename channel
    .doc = Renames a text channel.
    .w-only-the-workspace-s-moderators-rename = Only the workspace's moderators rename a channel
    .w-name-the-channel-first = Name the channel first

d-k5m36ngd-name = Name

d-tekxv77d = Rename voice channel
    .doc = Renames a voice channel.
    .w-only-the-workspace-s-moderators-rename = Only the workspace's moderators rename a channel
    .w-name-the-channel-first = Name the channel first

d-tekxv77d-name = Name

d-sgvix76a = Set the topic
    .doc = Says what a channel is for, in its topic.
    .w-only-the-workspace-s-moderators-set = Only the workspace's moderators set a channel's topic

d-sgvix76a-topic = Topic

d-ciefrn2e = Rename category
    .doc = Renames a category.
    .w-only-the-workspace-s-moderators-rename = Only the workspace's moderators rename a category
    .w-name-the-category-first = Name the category first

d-ciefrn2e-name = Name

d-z7ux3t5k = Archive channel
    .doc = Sets a channel aside, out of the sidebar, keeping everything said in it, or brings it back.
    .w-only-the-workspace-s-moderators-archive = Only the workspace's moderators archive a channel
    .w-archive-channel = Archive channel
    .w-restore-channel = Restore channel

d-z7ux3t5k-archived = Archived

d-yjc4fmrk = Edit
    .doc = Changes what a message says.
    .w-only-its-author-can-edit-a = Only its author can edit a message
    .w-type-a-message-first = Type a message first

d-yjc4fmrk-body = Body

d-yjc4fmrk-turn = Turn

d-vhmhrq2t = Delete
    .doc = Deletes a message from the channel or thread it is in.

d-vhmhrq2t-to = To

d-vhmhrq2t-m = M

d-nzltze4g = Pin
    .doc = Pins a message to its channel, or unpins it.
    .w-pin = Pin
    .w-unpin = Unpin

d-nzltze4g-pinned = Pinned

d-ia5g3jsz = React
    .doc = Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.

d-ia5g3jsz-emoji = Emoji

d-7sjai347 = Report
    .doc = Reports a message of a conversation to the workspace's moderators, saying why when the reporter does.
    .did = reported a message

d-7sjai347-to = To

d-7sjai347-m = M

d-7sjai347-reason = Reason

d-honuhrmf = Decide
    .doc = Settles a report: what a moderator decided about it.
    .w-only-the-workspace-s-moderators-settle = Only the workspace's moderators settle a report
    .w-say-what-was-decided = Say what was decided

d-honuhrmf-decision = Decision

d-6uysx57s = Start thread
    .doc = Starts a thread from a message of a channel or a direct conversation, named after what the message says.

d-6uysx57s-c = C

d-6uysx57s-m = M

d-6uysx57s-name = Name

d-l6xtwtlq = Bridge~present

d-2yeh74ot = New skill~present

d-ujq3oyls = Correct skill~present

d-hpamj3eq = Keep example~present

d-amkgebye = Report problem~present

d-pszgxdax = Message person~present

d-vib4vsqe = Send~present

d-dgrvpxre = Send view~present

d-psjj65el = Relay~present

d-zmf3ccig = Relay reaction~present

d-mdviqjz2 = Delete channel~present

d-czomdsi6 = Delete category~present

d-r47ecljq = Rename channel~present

d-cqfpcwyx = Rename voice channel~present

d-maq2lipz = Set topic~present

d-vrzrke7n = Rename category~present

d-g5hhjpdz = Archive channel~present

d-7ioawutl = Edit~present

d-p5ahaaux = Pin~present

d-rcr57dzk = Decide~present

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
    .w-everyone = Everyone
    .w-workspace = Workspace
    .w-workspace-skills = Workspace skills
    .w-no-channel-open = No channel open

d-ymtess4x = Channel
    .w-pinned-messages = Pinned messages
    .w-channel = Channel
    .w-rename-channel = Rename channel
    .w-set-the-topic = Set the topic
    .w-restore-channel = Restore channel
    .w-archive-channel = Archive channel
    .w-message = Message #{ $name }
    .w-attach = Attach
    .w-send = Send
    .w-wrote-this-in = { $name } wrote this in { $service }
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit

d-jbezqvlc = Direct
    .w-message = Message { $name }
    .w-attach = Attach
    .w-send = Send
    .w-wrote-this-in = { $name } wrote this in { $service }
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit

d-q45utnmh = Channel summary

d-i3f4daaw = Pins
    .w-nothing-pinned-yet = Nothing pinned yet
    .w-pin-a-message-from-its-menu = Pin a message from its menu to keep it here for everyone.

d-pyomqal2 = People
    .w-people = People
    .w-agent = Agent
    .w-message = Message

d-qnkkcusk = Voice

d-lkjppscz = Voice summary

d-enkldjck = Direct summary

d-oqg63mfd = Category summary

d-hrbsxhor = Skill row

d-enkgjyn7 = Thread
    .w-message = Message { $name }
    .w-attach = Attach
    .w-send = Send
    .w-wrote-this-in = { $name } wrote this in { $service }
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit

d-kd5tp6dr = Line run
    .w-running = Running…

d-rydgecpc = Typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-tx3zjawu = Direct typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-dcprca6g = Thread typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-zprw5jt4 = Persona
    .w-in = in { $written_in }

d-lzqy37mf = Turn steps

d-wysicvmq = Turn step
    .w-working = Working…
    .w-failed = Failed
    .w-allow = Allow
    .w-always = Always
    .w-deny = Deny

d-h327uiql = Turn

d-hjw5xd6b = Said
    .w-a-view-to-add-to-your = A view, to add to your own views
    .w-edited = edited
