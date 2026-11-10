# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-mail = Mail

d-weesm3yb = Account
    .doc = What the server of one of the workspace's mail accounts holds.
    .noun = { $count ->
        [one] account
       *[other] accounts
    }

d-yujnud5c = Key

d-4lo3ei5n = Address

d-j6nqqrit = Name

d-b5k7mvka = Trouble

d-33f3rqw2 = Online

d-jxc5rufs = Signature
    .doc = What the account's messages end with: the person's own, per account, as every mail client signs a message.

d-ehmoyzk2 = Mailboxes

d-qybqg4dl = Threads

d-weesm3yb-filter-online = Online

d-ecbhcqdr = Mailbox
    .doc = A mailbox of an account.
    .noun = { $count ->
        [one] mailbox
       *[other] mailboxes
    }

d-mh47w3p6 = Key

d-tlasysm5 = Name

d-xhiqdk3p = Role

d-dtd4czyv = Unread

d-rxoe5vzb = Total

d-ecbhcqdr-filter-unread = Unread

d-ecbhcqdr-filter-total = Total

d-xnczt27x = Thread
    .doc = A conversation: messages that answer each other, newest last.
    .noun = { $count ->
        [one] thread
       *[other] threads
    }

d-ecmpoary = Key

d-zu23m2dm = Account

d-u732lnuk = Subject

d-o3knjgos = Latest

d-sxcvmlon = Snippet

d-vyc7aj7v = Senders

d-izafodav = Count

d-56lwowdg = Unread

d-abe6c5jl = Flagged

d-3vusnp7i = Boxes

d-ueyfpzep = Messages

d-gy7z7du3 = Labels
    .doc = The labels people gave it (Gmail's and Fastmail's): the person's own, never the server's.

d-bdoh33hs = Snoozed until
    .doc = Snoozed until this moment: out of the inbox until then, and back once it has come, with no window open.

d-z7qxz544 = Muted
    .doc = Muted: the conversation stays out of the inbox while new messages keep coming, as a busy conversation is silenced, and it is still found under Muted and All mail.

d-xnczt27x-filter-inbox = Inbox

d-xnczt27x-filter-snoozed = Snoozed

d-xnczt27x-filter-labels = Labels

d-xnczt27x-filter-flagged = Flagged

d-xnczt27x-filter-muted = Muted

d-xnczt27x-filter-sent = Sent

d-xnczt27x-filter-archive = Archive

d-xnczt27x-filter-junk = Junk

d-xnczt27x-filter-trash = Trash

d-xnczt27x-filter-unread = Unread

d-5ynnrlxh = Label
    .doc = A label the person gave their conversations: their own, never the server's, as Gmail's and Fastmail's are.
    .noun = { $count ->
        [one] label
       *[other] labels
    }

d-ammoua3h = Name

d-sshtf3z7 = Message
    .doc = A message.
    .noun = { $count ->
        [one] message
       *[other] messages
    }

d-kzjq6hvc = Key

d-kdxoo3c6 = Account

d-ntqgnz4f = Subject

d-l3nc65iv = From

d-mo6l36a3 = From name

d-46k23iwq = To

d-bgzxugi5 = Cc

d-tfxwkfn2 = Received

d-sbrol5fc = Snippet

d-4bchz3ch = Body

d-anuds3qt = Message id

d-asu6ymha = References

d-4zhzrkne = Attachments

d-ubfcf6ep = Unread

d-vxxb4sqr = Flagged

d-xlejsz5j = Mailbox

d-sshtf3z7-filter-received = Received

d-sshtf3z7-filter-unread = Unread

d-sshtf3z7-filter-flagged = Flagged

d-lidkonvr = Draft
    .doc = A message being written: who it goes to, what it says, what it answers, and how it stands on its way out.
    .noun = { $count ->
        [one] draft
       *[other] drafts
    }
    .untitled = New message

d-m2g6zrle = From

d-hd3o4gkq = To

d-cwukhrvn = Cc

d-tyxmravh = Subject

d-d5u2c7jm = Body

d-w6twdkob = In reply to

d-w26tzvt4 = References

d-yfyt2cfh = Sent

d-lgrwf5yf = Send at
    .doc = When it goes, and `Time` is never empty, so a draft not sent yet says the epoch.

d-h2nvr2op = Recalled
    .doc = It was taken back before it went: it never leaves, and it is a draft again.

d-lidkonvr-filter-drafts = Drafts

d-lidkonvr-filter-sent = Sent

d-su4h67i5 = Mail
    .doc = The person's mail: the accounts the workspace's settings hold, what they are writing, and the labels they gave their conversations.
    .noun = { $count ->
        [one] mail
       *[other] mails
    }

d-blig7xcq = Accounts

d-gkuo27e6 = Drafts

d-z3zcztpz = Labels

d-4ow5ydy5 = Hold
    .doc = How many seconds a sent message waits before it goes, so it can be taken back: the person's setting, ten by default, and what rules out holding it not at all.

d-su4h67i5-filter-hold = Hold

d-ejo2352x = Signature
    .doc = Sets what the account signs its messages with, or clears it: the person's own, per account.

d-arnkrvgw = Check now
    .doc = Asks the account's server for its mail now: what a person presses when the window says the server could not be reached, or that what it fetched could not be saved.

d-eon6gbrn = New message
    .doc = Starts a message from the account.

d-ygbvdl5l = New message
    .doc = Starts a message from the first account, where there is one: a window with no account offers nothing to press and says where an account comes from, rather than a control that cannot run.

d-hirareiw = Reply
    .doc = Answers the conversation's newest message, to whoever wrote it, quoting it under what is written.

d-72f72tm7 = Reply all
    .doc = Answers the conversation's newest message, to everyone it went to, quoting it.

d-h5fphq56 = Reply to
    .doc = Answers one message of the conversation, to whoever wrote it, quoting it.

d-ym62maet = Reply all
    .doc = Answers one message of the conversation, to everyone it went to but the person answering.

d-jokcwung = Forward message
    .doc = Sends one message of the conversation on, with its sender, when and what it says.

d-q6ltvsjf = Forward
    .doc = Sends the conversation's newest message on, with its sender and what it says.

d-eqvgypxr = Archive
    .doc = Archives the conversation: its messages leave the inbox.

d-eef4pxgf = Move to inbox
    .doc = Moves the conversation back to the inbox, and unmutes it: what a person brings back is what they want to see again.

d-taywgw3n = Move to
    .doc = Moves the conversation to another mailbox of its account.

d-xytb2lle = Delete
    .doc = Deletes the conversation: its messages go to the trash.

d-le4lvs2c = Flag
    .doc = Flags or unflags the conversation.

d-6bwt463c = Snooze
    .doc = Puts the conversation away until a moment: it leaves the inbox, and comes back once that moment has come, with no window open.

d-kl4ikisg = Snooze an hour
    .doc = Puts the conversation away for an hour, the snooze people reach for most.

d-7icvl7qs = Snooze until tomorrow
    .doc = Puts the conversation away until tomorrow morning, at nine, in the person's own zone.

d-wl2c3dxh = Bring back
    .doc = Brings a snoozed conversation back now, whatever its moment was: it is in the inbox again.

d-ck5k6uop = Label
    .doc = Gives the conversation a label, or takes one away: `on` says which.

d-vydc2hnq = Rename label
    .doc = Renames one of the person's labels, all the conversations that carry it with it.

d-phsy7vhs = Delete label
    .doc = Deletes one of the person's labels, and takes it off every conversation that carries it.

d-xp4s2kl7 = Cancel scheduled send
    .doc = Brings a message back to the outbox: it was sent to go at a moment still to come, and it is a draft again, whatever the moment.

d-xxxfkczs = Send later
    .doc = Sends the message at a moment instead of now: it waits in the outbox until then, as the person's hold makes a message wait, and goes with no window open.

d-jysss72s = Send tomorrow morning
    .doc = Sends the message at nine tomorrow, the moment people reach for most when they write in the evening and do not want it to arrive then.

d-sqtuwk6b = Mark all read
    .doc = Marks every conversation of the account read.

d-gkqpzio6 = Mute
    .doc = Mutes the conversation, or unmutes it: a muted one stays out of the inbox until it is brought back, whatever its new messages do.

d-trjwm3gb = Mark as unread
    .doc = Marks the conversation read, or unread again.

d-7zk6ua6p = Send from
    .doc = Sends the message from another of the person's accounts.

d-qrcwx7a6 = Delete draft
    .doc = Throws a message being written away.

d-qcfc2rhb = Send
    .doc = Sends the message from its account.

d-twebu5cb = Undo send
    .doc = Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was.

d-a4rv5wm6 = Answer
    .doc = Answers the conversation with what was typed: the message goes to whoever wrote its newest, as a reply to it, quoted.

d-gcixhwyt = New draft~present

d-bj7uen6f = Archive~present

d-bs42bxdn = To inbox~present

d-wfvq5lde = Move to~present

d-efageosr = Delete~present

d-cvmtfflh = Flag~present

d-3ihnegs3 = Snooze~present

d-p5knuykz = Label~present

d-cstttevp = Rename label~present

d-cxnwxvyu = Cancel later~present

d-lnirkyf7 = Send later~present

d-5zi36ozt = Mute~present

d-xnse2xt5 = Mark read~present

d-iv7e7g52 = Send from~present

d-czouixzd = Send~present

d-i3cueozg = Undo send~present

d-4hds3dwy = Answer~present

d-5wix2354 = Compose
    .w-to = To { $to }

d-7rcrynvm = Mail
    .w-add-a-mail-account-in-settings = Add a mail account in Settings

d-4fjgzcq6 = Unified
    .w-all-accounts = All accounts
    .w-all-inboxes = All inboxes
    .w-all-mail = All mail
    .w-labels = Labels
    .w-could-not-save-what-it-fetched = { $name } could not save what it fetched
    .w-can-t-reach-showing-what-was = Can't reach { $name }. Showing what was last fetched.
    .w-try-again = Try again

d-gka45our = Mailboxes
    .w-all-mail = All mail
    .w-labels = Labels
    .w-could-not-save-what-it-fetched = { $name } could not save what it fetched
    .w-can-t-reach-showing-what-was = Can't reach { $name }. Showing what was last fetched.
    .w-try-again = Try again

d-zp2cy2sc = Inbox
    .w-could-not-save-what-it-fetched = { $name } could not save what it fetched
    .w-can-t-reach-showing-what-was = Can't reach { $name }. Showing what was last fetched.
    .w-try-again = Try again
    .w-nothing-here = Nothing here

d-jgdzx3la = Thread list
    .w-nothing-here = Nothing here

d-u43u7z6w = Thread row

d-77noqogh = Draft row

d-bbuecgsi = Conversation
    .w-reply-to = Reply to { $subject }
    .w-reply = Reply

d-n2xgxnbw = Message
    .w-reply = Reply
    .w-reply-all = Reply all
    .w-forward = Forward
    .w-to = to { $to }
    .w-hide-quoted-text = Hide quoted text
    .w-show-quoted-text = Show quoted text
    .w-attached = Attached: { $attachments }

d-oqpazwvc = Editor
    .w-sending = Sending
    .w-it-goes-in-a-moment-take = It goes in a moment. Take it back if you need to.
    .w-undo-send = Undo send
    .w-name-example-org = name@example.org
    .w-subject = Subject
    .w-write-your-message = Write your message
