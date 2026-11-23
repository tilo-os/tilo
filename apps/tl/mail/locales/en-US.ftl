# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-mail = Mail

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

d-hzbwof46 = Fetched
    .doc = When the account's mail was last taken in from its server, as the clock the person read it at: 1970 when it never was.

d-jxc5rufs = Signature
    .doc = What the account's messages end with.

d-yinmk5zb = Identities
    .doc = The other names and addresses the account sends as, beside its own, one `Name <address>` a line: what a person writes from when they write as a role rather than as themselves.

d-ehmoyzk2 = Mailboxes

d-qybqg4dl = Threads

d-weesm3yb-filter-online = Online

d-weesm3yb-filter-fetched = Fetched

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

d-al2vjuet = More
    .doc = Whether the account's server holds messages older than the ones this workspace holds: what "Show older messages" reads the page before them for.

d-ecbhcqdr-filter-unread = Unread

d-ecbhcqdr-filter-total = Total

d-ecbhcqdr-filter-more = More

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
    .doc = The labels members assigned to this conversation in the workspace.

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

d-xnczt27x-filter-latest = Received

d-5ynnrlxh = Label
    .doc = A workspace label for conversations.
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

d-plb4mg2p = Html
    .doc = What it says as its sender formatted it: its HTML part, the pictures it shows in place read from this device's content.

d-anuds3qt = Message id

d-asu6ymha = References

d-yltl32r3 = Attachments
    .doc = The files it came with, each kept in the workspace's content: one whose bytes are not on this device yet is named with no content.

d-ubfcf6ep = Unread

d-vxxb4sqr = Flagged

d-xlejsz5j = Mailbox

d-hgysjvfy = Original
    .doc = The content reference of the message as it was sent, for one taken in from outside the account's server (`deliver_message`): empty for one its server holds.

d-wuefabwr = Parts
    .doc = Where each large part of a message taken in from outside stands in `original` and the pieces of content it is in, one JSON text a line: empty for one its server holds.

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

d-zx2b4ax2 = From address
    .doc = The identity of `from` it goes from, one of the account's own (`Name <address>`): empty for the account's own address, and for one it no longer sends as.

d-hd3o4gkq = To

d-cwukhrvn = Cc

d-ib44zkec = Bcc
    .doc = Blind recipients, who see the message but are not named in it.

d-tyxmravh = Subject

d-d5u2c7jm = Body

d-w6twdkob = In reply to

d-w26tzvt4 = References

d-yfyt2cfh = Sent

d-lgrwf5yf = Send at
    .doc = When it goes: 1970 for a draft not sent yet.

d-h2nvr2op = Recalled
    .doc = It was taken back before it went: it never leaves, and it is a draft again.

d-em6rrmae = Invitation
    .doc = The calendar invitation it carries (iCalendar), which goes beside what it says so the guest's calendar can answer it: empty for none.

d-2tq2zegc = Attachments
    .doc = The files it takes along.

d-ozukvva2 = Set aside
    .doc = What the native Mail kept on this draft that this one has no place for (attachments, private notes), said in words: never sent with it.

d-lidkonvr-filter-drafts = Drafts

d-lidkonvr-filter-sent = Sent

d-su4h67i5 = Mail
    .doc = The workspace mailbox holds its accounts, drafts, and conversation labels.
    .noun = { $count ->
        [one] mail
       *[other] mails
    }

d-blig7xcq = Accounts

d-gkuo27e6 = Drafts

d-z3zcztpz = Labels

d-bag7bf25 = Recovered mail
    .doc = Conversations kept outside the IMAP account collections: messages delivered from outside an account's server, and the mail of an account that is gone.

d-4ow5ydy5 = Hold
    .doc = How many seconds members have to undo a send.

d-su4h67i5-filter-hold = Hold

d-ejo2352x = Signature
    .doc = Sets or clears the workspace account's signature.

d-ejo2352x-signature = Signature

d-546ahgq6 = Undo send time
    .doc = Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait at all at zero.
    .w-a-message-cannot-wait-less-than = A message cannot wait less than no time

d-546ahgq6-seconds = Seconds

d-arnkrvgw = Check now
    .doc = Asks the account's server for its mail now: what a person presses when the window says the server could not be reached, or that what it fetched could not be saved.

d-wgbxl4or = Show older messages
    .doc = Asks the account's server for the messages older than the oldest this workspace holds in the mailbox, so a folder longer than one page can be read back to its first message.

d-eon6gbrn = New message
    .doc = Starts a message from the account.

d-v7wxxiaj = Carry native mail
    .doc = The mailbox the native Mail kept before this app took its id over, carried into this one once: every draft it was writing and every message still waiting in its outbox is a draft here, from the same account, as the same identity, to the same people (the blind ones too), with the same subject, body and invitation.

d-ygbvdl5l = New message
    .doc = Starts a message from the first account.
    .w-add-a-mail-account-in-settings = Add a mail account in Settings first

d-lnhvmglv = Draft a message
    .doc = Writes a message for the person to read and send: to whom, its subject and what it says, answering `in_reply_to` (a Message-ID) when it names one.
    .w-it-goes-to-nobody-yet = It goes to nobody yet

d-lnhvmglv-to = To

d-lnhvmglv-subject = Subject

d-lnhvmglv-body = Body

d-lnhvmglv-cc = Cc

d-lnhvmglv-in_reply_to = In reply to

d-lnhvmglv-from = From

d-hom44or2 = Invite by mail
    .doc = Starts a message inviting `to` to an event, its invitation (`calendar_ics`, iCalendar) carried beside what it says: what Calendar hands Mail for each guest it invites.
    .w-add-a-mail-account-in-settings = Add a mail account in Settings first
    .w-an-invitation-goes-to-someone = An invitation goes to someone
    .w-the-invitation-is-not-a-calendar = The invitation is not a calendar request

d-hom44or2-to = To

d-hom44or2-subject = Subject

d-hom44or2-body = Body

d-hom44or2-calendar_ics = Calendar ics

d-grphua5j = Deliver a message
    .doc = Takes in a message that reached the workspace from outside its accounts' servers: one moved in from another mail program (`tilo migrate`), or handed over through the API.
    .w-a-message-says-who-it-is = A message says who it is from

d-grphua5j-from = From

d-grphua5j-subject = Subject

d-grphua5j-body = Body

d-grphua5j-folder = Folder

d-grphua5j-read = Read

d-grphua5j-to = To

d-grphua5j-cc = Cc

d-grphua5j-date = Date

d-grphua5j-conversation = Conversation

d-grphua5j-message_id = Message id

d-grphua5j-original = Original

d-grphua5j-parts = Parts

d-grphua5j-attachments = Attachments

d-tr43xnyj = Export as Markdown
    .doc = The message as a Markdown file: what a person exports to read it, or keep it, elsewhere.

d-shpafbd7 = Export as JSON
    .doc = The message as a JSON file: what a script or an agent reads its record from.

d-ydo5gq5e = Export as .eml
    .doc = The message as an `.eml` file, which every mail program opens.

d-3vrmj56n = Open a message file
    .doc = Opens a message kept as a file a mail program writes (`.eml`, a single message of an mbox, or Apple Mail's `.emlx`): its headers are read, and it is kept with the mail held outside the accounts, read, in the inbox.
    .w-the-file-holds-no-message = The file holds no message

d-3vrmj56n-file = File

d-hirareiw = Reply
    .doc = Answers the conversation's newest message, to whoever wrote it, quoting it under what is written.

d-72f72tm7 = Reply all
    .doc = Answers the conversation's newest message, to everyone it went to, quoting it.

d-h5fphq56 = Reply to
    .doc = Answers one message of the conversation, to whoever wrote it, quoting it.

d-ym62maet = Reply all
    .doc = Answers one message of the conversation, to everyone it went to but the person answering.

d-jokcwung = Forward message
    .doc = Sends one message of the conversation on, with its sender, when and what it says, and the files it came with.

d-q6ltvsjf = Forward
    .doc = Sends the conversation's newest message on, with its sender, what it says and the files it came with.

d-ipew27sc = Save attachment
    .doc = One file the message came with, as a file of its own: what Save puts in Files, and what an agent reads or hands another app.
    .w-it-came-with-no = It came with no { $name }
    .w-is-not-downloaded-yet = { $name } is not downloaded yet

d-ipew27sc-name = Name

d-dshny4x3 = Attach a file
    .doc = Takes a file along with the message: one the person picks, or one an agent hands over.
    .w-this-message-went-already = This message went already
    .w-a-message-takes-up-to-16 = A message takes up to 16 files
    .w-is-larger-than-32-mb = { $name } is larger than 32 MB

d-dshny4x3-file = File

d-jk7ep3bb = Remove attachment
    .doc = Leaves a file the message was to take along behind.
    .w-this-message-went-already = This message went already
    .w-it-takes-no = It takes no { $name }

d-jk7ep3bb-name = Name

d-eqvgypxr = Archive
    .doc = Archives the conversation: its messages leave the inbox.
    .w-it-is-not-in-the-inbox = It is not in the inbox

d-eef4pxgf = Move to inbox
    .doc = Moves the conversation back to the inbox, and unmutes it: what a person brings back is what they want to see again.
    .w-it-is-in-the-inbox = It is in the inbox

d-taywgw3n = Move to
    .doc = Moves the conversation to another mailbox of its account.
    .w-it-is-in-already = It is in { $name } already

d-taywgw3n-to = To

d-xytb2lle = Delete
    .doc = Deletes the conversation: its messages go to the trash.
    .w-it-is-in-the-trash = It is in the trash

d-pz2ntes2 = Delete permanently
    .doc = Deletes the conversation's trashed messages for good, from its server too, and the conversation with them when no message of it is left.
    .w-it-is-not-in-the-trash = It is not in the trash
    .w-nothing-of-it-is-in-the = Nothing of it is in the trash

d-le4lvs2c = Flag
    .doc = Flags or unflags the conversation.
    .w-unflag = Unflag
    .w-flag = Flag

d-le4lvs2c-flagged = Flagged

d-6bwt463c = Snooze
    .doc = Puts the conversation away until a moment: it leaves the inbox, and comes back once that moment has come, with no window open.
    .w-pick-a-moment-still-to-come = Pick a moment still to come

d-6bwt463c-until = Until

d-kl4ikisg = Snooze an hour
    .doc = Puts the conversation away for an hour, the snooze people reach for most.

d-7icvl7qs = Snooze until tomorrow
    .doc = Puts the conversation away until tomorrow morning, at nine, in the person's own zone.

d-wl2c3dxh = Bring back
    .doc = Brings a snoozed conversation back now, whatever its moment was: it is in the inbox again.

d-ck5k6uop = Label
    .doc = Gives the conversation a label, or takes one away: `on` says which.
    .w-a-label-needs-a-name = A label needs a name

d-ck5k6uop-name = Name

d-ck5k6uop-on = On

d-vydc2hnq = Rename label
    .doc = Renames one of the workspace's labels, all the conversations that carry it with it.
    .w-a-label-needs-a-name = A label needs a name
    .w-you-have-a-label = You have a label { $name }

d-vydc2hnq-l = L

d-vydc2hnq-name = Name

d-phsy7vhs = Delete label
    .doc = Deletes one of the workspace's labels, and takes it off every conversation that carries it.

d-phsy7vhs-l = L

d-xp4s2kl7 = Cancel scheduled send
    .doc = Brings a message back to the outbox: it was sent to go at a moment still to come, and it is a draft again, whatever the moment.
    .w-it-is-not-waiting-to-go = It is not waiting to go
    .w-it-went-already = It went already
    .w-scheduled-send-cancelled = Scheduled send cancelled

d-xxxfkczs = Send later
    .doc = Sends the message at a moment instead of now: it waits in the outbox until then, as the person's hold makes a message wait, and goes with no window open.
    .w-this-message-went-already = This message went already
    .w-it-goes-to-nobody-yet = It goes to nobody yet
    .w-add-a-mail-account-in-settings = Add a mail account in Settings first
    .w-pick-a-moment-still-to-come = Pick a moment still to come

d-xxxfkczs-at = At

d-jysss72s = Send tomorrow morning
    .doc = Sends the message at nine tomorrow, the moment people reach for most when they write in the evening and do not want it to arrive then.

d-sqtuwk6b = Mark all read
    .doc = Marks every conversation of the account read.

d-gkqpzio6 = Mute
    .doc = Mutes the conversation, or unmutes it: a muted one stays out of the inbox until it is brought back, whatever its new messages do.
    .w-unmute = Unmute
    .w-mute = Mute

d-gkqpzio6-muted = Muted

d-trjwm3gb = Mark as unread
    .doc = Marks the conversation read, or unread again.
    .w-mark-as-read = Mark as read
    .w-mark-as-unread = Mark as unread

d-trjwm3gb-unread = Unread

d-hk43bpjp = Read on open
    .doc = Opening a conversation reads it, wherever it is opened from: the list, search, a link or an agent.

d-7zk6ua6p = Send from
    .doc = Sends the message from another workspace account.
    .w-this-message-went-already = This message went already

d-7zk6ua6p-a = A

d-naegttbl = Send as
    .doc = Sends the message as another of its account's names or addresses, `Name <address>`: what a person writing as a role rather than as themselves chooses.
    .w-this-message-went-already = This message went already

d-naegttbl-identity = Identity

d-qrcwx7a6 = Delete draft
    .doc = Throws a message being written away.

d-qrcwx7a6-d = D

d-qcfc2rhb = Send
    .doc = Sends the message from its account.
    .w-this-message-went-already = This message went already
    .w-it-goes-to-nobody-yet = It goes to nobody yet
    .w-add-a-mail-account-in-settings = Add a mail account in Settings first

d-twebu5cb = Undo send
    .doc = Takes back a message that was sent and has not gone yet: it never leaves, and the message is a draft again, as it was.
    .w-it-has-not-been-sent = It has not been sent
    .w-it-went-already = It went already
    .w-taken-back-before-it-went = Taken back before it went

d-a4rv5wm6 = Answer
    .doc = Answers the conversation with what was typed: the message goes to whoever wrote its newest, as a reply to it, quoted.
    .w-say-something-first = Say something first
    .w-the-account-for-this-conversation-is = The account for this conversation is unavailable

d-a4rv5wm6-body = Body

d-nxayphhi = Message file
    .w-message = Message

d-t57taubn = Forwarded
    .w-not-downloaded-so-not-sent-on = Not downloaded, so not sent on: { $names }

d-dqdt6fce = Place title
    .w-all-mail = All mail

d-k2js7gyd = Delivery says
    .w-taken-back-and-a-draft-again = Taken back, and a draft again
    .w-sent = Sent
    .w-sending = Sending
    .w-not-sent = Not sent: { $reason }
    .w-not-known-whether-it-was-sent = Not known whether it was sent
    .w-sending-in-a-moment = Sending in a moment
    .w-waiting-to-send = Waiting to send
    .w-waiting-to-go-online = Waiting to go online: { $reason }

d-2zkppx6r = Set hold~present

d-gcixhwyt = New draft~present

d-lehcqyhl = Draft message~present

d-7jf4pn3j = Prepare calendar invitation~present

d-s2qkkueu = Deliver message~present

d-vdfbsj4a = Open message file~present

d-swcww42a = Attachment~present

d-fxtnru4s = Attach~present

d-p76r6z3v = Detach~present

d-bj7uen6f = Archive~present

d-bs42bxdn = To inbox~present

d-wfvq5lde = Move to~present

d-efageosr = Delete~present

d-djmtvfs7 = Purge~present

d-cvmtfflh = Flag~present

d-3ihnegs3 = Snooze~present

d-p5knuykz = Label~present

d-cstttevp = Rename label~present

d-cxnwxvyu = Cancel later~present

d-lnirkyf7 = Send later~present

d-5zi36ozt = Mute~present

d-xnse2xt5 = Mark read~present

d-iv7e7g52 = Send from~present

d-5kuylc3o = Send as~present

d-czouixzd = Send~present

d-i3cueozg = Undo send~present

d-4hds3dwy = Answer~present

d-5wix2354 = Compose
    .w-to = To { $to }
    .w-bcc = Bcc { $bcc }

d-dhradxj7 = Account notice
    .w-can-t-save-what-fetched = Can't save what { $name } fetched
    .w-can-t-reach = Can't reach { $name }
    .w-last-fetched = { $reason }. Last fetched { $fetched }
    .w-try-again = Try again

d-7rcrynvm = Mail
    .w-add-a-mail-account-in-settings = Add a mail account in Settings
    .w-recovered-mail = Recovered mail
    .w-mail = Mail

d-4fjgzcq6 = Unified
    .w-recovered-mail = Recovered mail
    .w-all-accounts = All accounts
    .w-all-inboxes = All inboxes
    .w-all-mail = All mail
    .w-labels = Labels

d-3k7hkyhl = Recovered mail

d-m5y6nl2m = All inboxes
    .w-add-a-mail-account-in-settings = Add a mail account in Settings
    .w-is-inbox = is:inbox

d-s7tarqlp = Accounts threads

d-tfom3oeo = Account threads
    .w-show-older-messages = Show older messages

d-gka45our = Mailboxes
    .w-all-mail = All mail
    .w-labels = Labels

d-zp2cy2sc = Inbox
    .w-is = is:{ $key }
    .w-nothing-here = Nothing here

d-jgdzx3la = Thread list
    .w-nothing-here = Nothing here

d-u43u7z6w = Thread row

d-d6irjool = Thread item
    .w-preview = Preview

d-77noqogh = Draft row

d-bbuecgsi = Conversation
    .w-reply-to = Reply to { $subject }
    .w-reply = Reply

d-krn2y5tw = Conversation messages

d-n2xgxnbw = Message
    .w-reply = Reply
    .w-reply-all = Reply all
    .w-forward = Forward
    .w-to = to { $to }
    .w-hide-quoted-text = Hide quoted text
    .w-show-quoted-text = Show quoted text

d-llqqdcy5 = Attachment chip
    .w-not-downloaded = { $name }, not downloaded

d-u5uuebo4 = Inspect
    .w-no-subject = No subject
    .w-unread = Unread
    .w-it-stays-unread-until-its-conversation = It stays unread until its conversation is opened.
    .w-to = to { $to }

d-oqpazwvc = Editor
    .w-status = Status
    .w-sending = Sending
    .w-it-goes-in-a-moment = It goes in a moment.
    .w-undo-send = Undo send
    .w-from = From
    .w-no-mail-account-to-send-from = No mail account to send from
    .w-add-an-account-in-settings = Add an account in Settings
    .w-to = To
    .w-name-example-org = name@example.org
    .w-cc = Cc
    .w-bcc = Bcc
    .w-subject = Subject
    .w-bold = Bold
    .w-italic = Italic
    .w-underline = Underline
    .w-bulleted-list = Bulleted list
    .w-link = Link
    .w-https-example-org = https://example.org
    .w-add-link = Add link
    .w-write-your-message = Write your message
    .w-attach = Attach
    .w-send = Send
