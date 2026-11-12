# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-chat = Chat

d-lmdoxbnn = Conversations
    .doc = The workspace's conversations in the sidebar's order: channels outside any category, the categories each with its own, and the direct messages, in the list as people.
    .noun = { $count ->
        [one] conversations
       *[other] conversationses
    }

d-5kzdptyg = Channels

d-fe2st4jb = Category
    .doc = A named group of channels in the sidebar.
    .noun = { $count ->
        [one] category
       *[other] categories
    }

d-hbhrds5c = Name

d-hvqgmwrk = Channels
    .doc = The server's channels.

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
    .doc = What a message said before messages were rich text, kept as it was written.

d-4zr7hti6 = Pinned
    .doc = Whether it is pinned to its channel, which search finds.

d-ofad24rc = Reactions
    .doc = Who reacted, by emoji.

d-dh6346no = Sent view
    .doc = A view the message sends, as its sender had it among their views: its tilolang definition, which a reader adds to their own views (Views' `add`), its author whoever sent the message.

d-hk2t6bvk-filter-pinned = Pinned

d-6qpql3pg = Thread
    .doc = A side conversation branched off one message.
    .noun = { $count ->
        [one] thread
       *[other] threads
    }

d-xuvfifa5 = Name

d-ijy454g6 = Messages

d-2ki7h2z5 = Archived

d-6qpql3pg-filter-archived = Archived

d-5nkbpj6q = Direct
    .doc = A private conversation: one other person, or a named group of them, in the same message model as a channel.
    .noun = { $count ->
        [one] direct
       *[other] directs
    }

d-5stv6ro7 = Name

d-ycadyqh5 = Person
    .doc = The one person it is with, for a direct message.

d-y65whtrn = Messages

d-3p73kpuw = Threads

d-5nkbpj6q-filter-person = Person

d-yx5sbebu = Part
    .doc = A piece of a message from before messages were rich text: its text, or a block's source.
    .noun = { $count ->
        [one] part
       *[other] parts
    }

d-zevnbs3i = Kind
    .doc = `text` or `block`.

d-3hkzkqvr = Body

d-msf7yzn7 = New channel
    .doc = Adds a text channel to the sidebar.

d-26f7sh5k = New category
    .doc = Adds a category to the sidebar.

d-abono4ra = New channel
    .doc = Adds a text channel to a category.

d-3cetzzm3 = Message
    .doc = Opens the direct message with `who`, in the sidebar, or the one already open with them: a person has one conversation, not a new empty one each time.

d-peb7lz2j = Send
    .doc = Posts a message in a channel, a thread or a direct conversation as the invoker, answering another when `reply_to` names one.

d-qaa5kdra = Send a view
    .doc = Sends a view as a message: its title as what the message says, its definition for anyone who reads it to add to their own views.

d-255qjx7d = Edit
    .doc = Changes what a message says.

d-vhmhrq2t = Delete
    .doc = Deletes a message from the channel or thread it is in.

d-2ojqeubz = Pin
    .doc = Pins a message to its channel, or unpins it.

d-np3mn65k = React
    .doc = Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.

d-6uysx57s = Start thread
    .doc = Starts a thread from a message of a channel or a direct conversation, named after what the message says.

d-dgrvpxre = Send view~present

d-comllwls = Edit~present

d-c2x2a6dh = Pin~present

d-dzzuovny = Unread
    .w-unread = Unread
    .w-show-all = Show all { $n }

d-5rkfnkg4 = Sidebar
    .w-channels = Channels
    .w-no-channels-yet = No channels yet
    .w-new-channel = New channel
    .w-direct-messages = Direct messages
    .w-no-channel-open = No channel open

d-ymtess4x = Channel
    .w-message = Message #{ $name }
    .w-send = Send

d-jbezqvlc = Direct
    .w-message = Message { $name }
    .w-send = Send

d-q45utnmh = Channel summary

d-enkldjck = Direct summary

d-oqg63mfd = Category summary

d-enkgjyn7 = Thread
    .w-message = Message { $name }
    .w-send = Send

d-rydgecpc = Typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-tx3zjawu = Direct typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-dcprca6g = Thread typing here
    .w-is-typing = { $who } is typing…
    .w-and-others-are-typing = { $who } and others are typing…

d-hjw5xd6b = Said
    .w-a-view-to-add-to-your = A view, to add to your own views
    .w-edited = edited
