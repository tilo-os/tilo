# tilolang messages: generated from the package's source, commit it. Translate in
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

d-wsb4gtay = Discord
    .doc = The Discord accounts the workspace connected: their servers' channels and their direct messages are conversations here too.

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

d-fld77g72 = Summary
    .doc = What has been said here so far, in a few paragraphs, as an agent last wrote it down: what a turn reads in place of the messages it summarizes.

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

d-hj4axvbu = Turn
    .doc = The agent's turn that wrote it: the id of the `#answer` invocation it answers in, which Stop cancels and whose steps, questions and cost are the turn's.

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

d-5nkbpj6q-filter-people = People

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

d-iitgpqxl = Discord
    .doc = A connected Discord account: the server it bridges, what the bridge knows of both sides, and who here linked their Discord account.
    .noun = { $count ->
        [one] discord
       *[other] discords
    }

d-vljpdgnw = Name
    .doc = What the account is called on Discord.

d-kknc7wii = Guild id
    .doc = The server bridged, by its Discord id; empty until one is connected.

d-qdz6qkr6 = Server
    .doc = The server's name, as Discord shows it.

d-a4jyqij5 = Only channel
    .doc = The one channel bridged, by its Discord id; empty for the whole server.

d-5mjunvax = Mirror channels
    .doc = Channels made here after the server was adopted are made in Discord too.

d-uggd34ts = Map roles
    .doc = The workspace's roles are kept as roles the bridge owns in Discord.

d-kydsh6zv = Ownership
    .doc = Whether the server is still being brought in, or bridged both ways.

d-2q67a4ra = Token item
    .doc = The secret item holding the bot token on this device; the token never enters the resource.

d-n5ofv4eb = Bot user
    .doc = The bot user's Discord id: its own messages and reactions are echoes.

d-3dmimyt7 = Channels
    .doc = Discord's channels and categories as the bot last observed them.

d-d7fxjwt2 = Roles
    .doc = Discord's roles as the bot last observed them.

d-evnoxnvu = Emojis
    .doc = The server's custom emoji, as the bot last observed them.

d-k62ig5am = Map
    .doc = What joins a conversation here and a channel, category, thread, message, webhook, role or emoji there.

d-yualupio = Links
    .doc = The Discord accounts people here linked.

d-tzrgq6te = Link offers
    .doc = Discord accounts offered for linking, each to whoever holds its one-time token.

d-thwfqhxc = Connects
    .doc = People on their way through Discord's consent page, by person.

d-vtmfq4l3 = Sign in page
    .doc = Discord's consent page for signing in through this device, without its state: empty when the device offers none.

d-c4fmjipp = Sign in url
    .doc = Where a sign-in the person just started sends them, while it waits for what comes back.

d-xtjnoeq4 = Avatars
    .doc = A Discord avatar's hash and the address its image is kept at.

d-tmvf5t4d = Blocked personas
    .doc = Discord accounts whose messages are not mirrored.

d-dcuh5avh = Vanished
    .doc = Mapped Discord channels and roles seen once and gone from the latest layout.

d-mqxugvsk = Tilo only
    .doc = Conversations here that stay here: unmatched when the server was adopted, or their Discord side was deleted.

d-2unx5h27 = Discord only
    .doc = Discord channels and categories that stay there: their side here was deleted.

d-7ybcen5e = Voice problem
    .doc = Why the bridge cannot carry a voice channel's call now, as Discord refused it; empty while it can.

d-motcmqng = Emoji problem
    .doc = Why the workspace's emoji do not reach the server now; empty while they do.

d-p3wnnkta = Setup open
    .doc = Whether the setup dialog is open.

d-srjlhqfm = Setup servers
    .doc = The servers the bot is in, as finding them answered.

d-2dglfzgc = Setup problem
    .doc = Why finding them did not work, in words the person reads.

d-iitgpqxl-filter-mirror_channels = Mirror channels

d-iitgpqxl-filter-map_roles = Map roles

d-iitgpqxl-filter-ownership = Ownership

d-iitgpqxl-filter-blocked_personas = Blocked personas

d-iitgpqxl-filter-vanished = Vanished

d-iitgpqxl-filter-tilo_only = Tilo only

d-iitgpqxl-filter-discord_only = Discord only

d-iitgpqxl-filter-setup_open = Setup open

d-wjwii7hj = D channel
    .doc = A Discord channel, thread or category as the bridge last observed it.
    .noun = { $count ->
        [one] d channel
       *[other] d channels
    }

d-dt2462ze = Id

d-vwhfopxd = Kind

d-zujs26s2 = Name

d-jnsp3ms6 = Parent

d-wuiig67u = Position

d-pzuurh4n = Topic

d-7vqe34oe = Overwrites

d-wjwii7hj-filter-kind = Kind

d-wjwii7hj-filter-position = Position

d-icwr3kpv = Overwrite
    .doc = Who a channel's permission overwrite is for, and what it allows and denies.
    .noun = { $count ->
        [one] overwrite
       *[other] overwrites
    }

d-43slchtf = Id

d-jaz3sf34 = Kind
    .doc = 0 for a role, 1 for a member, as Discord numbers them.

d-wchwlbwm = Allow

d-o5qpdte4 = Deny

d-icwr3kpv-filter-kind = Kind

d-7zvrq2uv = D role
    .doc = A Discord role as the bridge last observed it.
    .noun = { $count ->
        [one] d role
       *[other] d roles
    }

d-ik6uney3 = Id

d-vwjlsiqe = Name

d-62enxr6z = Permissions

d-su5sqdby = Color

d-hfzxx5e4 = Position

d-6bxqrdb4 = Managed

d-7zvrq2uv-filter-color = Color

d-7zvrq2uv-filter-position = Position

d-7zvrq2uv-filter-managed = Managed

d-fxf3hlc6 = D emoji
    .doc = A Discord custom emoji.
    .noun = { $count ->
        [one] d emoji
       *[other] d emojis
    }

d-5catvpo2 = Id

d-xc7cmf4c = Name

d-msyn4klr = Animated

d-fxf3hlc6-filter-animated = Animated

d-7wtsum23 = Mapping
    .doc = What joins a conversation here to its Discord side.
    .noun = { $count ->
        [one] mapping
       *[other] mappings
    }

d-xenqumyt = Kind

d-h2kceqog = Tilo
    .doc = The conversation's, message's or role's published id here.

d-gvwrvf5h = Discord
    .doc = The Discord id it is carried to.

d-4j3epvkb = Origin

d-veylcb72 = Persona
    .doc = For a message Discord wrote: its author's account there.

d-3zjzfhzz = Root
    .doc = For a reply posted in a thread here: its thread's root, as Discord threads are one level.

d-bfytf6d2 = Picture
    .doc = For an emoji the bridge uploaded: the address of its picture.

d-5gviczcq = Said
    .doc = For a message carried there: what it last said there, so a change here is carried once.

d-rruvlxia = Emoji
    .doc = For a message carried there: the emoji the bot put on it there, each once.

d-7wtsum23-filter-kind = Kind

d-7wtsum23-filter-origin = Origin

d-7wtsum23-filter-emoji = Emoji

d-6urhdvjl = Link
    .doc = A Discord account someone here linked.
    .noun = { $count ->
        [one] link
       *[other] links
    }

d-qqa5u54y = Discord user

d-2tvmpdgj = Person
    .doc = The person here it is linked to.

d-tv6gectf = Linked at
    .doc = When it was linked, as ISO text.

d-hdxkp3hg = User name

d-prwxxpgw = Display name
    .doc = The name the account shows when that is another.

d-vkczwsqq = Avatar
    .doc = The account's Discord avatar hash.

d-zk6hstue = Interaction
    .doc = The slash command's reply that says what came of the link, when it was made from there.

d-tnavgag4 = Photo
    .doc = The avatar as a small picture, which the person's profile shows until they upload one.

d-zuaysaa2 = Photo avatar
    .doc = The avatar hash `photo` is of.

d-dwoj6lgz = Link offer
    .doc = A Discord account offered for linking, to whoever holds the one-time token it was offered with: only the token's check is kept.
    .noun = { $count ->
        [one] link offer
       *[other] link offers
    }

d-wpiftjgo = Check

d-je62xe7p = Discord user

d-j6pav34x = User name

d-nlxpfao3 = Display name

d-u4q32mi7 = Avatar

d-boe6b7bu = Person
    .doc = The person who started it here: only they may take it.

d-ob3qtfyi = Interaction
    .doc = The slash command it answers.

d-liyfeqjp = Expires
    .doc = When it runs out, as ISO text.

d-hxht6wij = Connect
    .doc = Someone on their way through Discord's consent page: the round trip's state, hashed, binds what comes back to them.
    .noun = { $count ->
        [one] connect
       *[other] connects
    }

d-3sdgerpb = Person

d-eyw2le7n = State sha256

d-qjuqlqxs = Expires
    .doc = When it runs out, as ISO text.

d-yom62ere = Server choice
    .doc = A server the bot is in.
    .noun = { $count ->
        [one] server choice
       *[other] server choices
    }

d-6sokcqur = Id

d-np6alf3a = Name

d-ll2vfvw4 = Server list
    .doc = The servers finding them answered.
    .noun = { $count ->
        [one] server list
       *[other] server lists
    }

d-pad4fk3d = Servers

d-w5l4kq6a = Send op
    .doc = A message through a channel's webhook, under the person's Discord name.
    .noun = { $count ->
        [one] send op
       *[other] send ops
    }

d-wzcpphmq = Op

d-fqkenlwb = Channel

d-5qmn6o6u = Webhook

d-35rsrlum = Thread

d-5f33mabo = Username

d-epcz6l7f = Avatar url

d-7exg3bha = Content

d-k7um3ati = Attachments

d-r52klxop = Created ms

d-ld3nm43w = Tilo
    .doc = The message here it carries, by its published id.

d-w5l4kq6a-filter-created_ms = Created ms

d-yxgnscic = Out attachment
    .doc = A file a message carries, by the address its bytes are kept at.
    .noun = { $count ->
        [one] out attachment
       *[other] out attachments
    }

d-agihkvk7 = Blob

d-ii75acbt = Name

d-irulcxme = Size

d-yxgnscic-filter-size = Size

d-hg5j4nr3 = Edit op
    .doc = A change to a message the bridge sent.
    .noun = { $count ->
        [one] edit op
       *[other] edit ops
    }

d-gufcumzw = Op

d-y7zty2fb = Channel

d-7vceotkp = Webhook

d-7wfpwki3 = Message

d-6pjwptoc = Thread

d-ah4d3qp4 = Content

d-b27uw4ku = Delete op
    .doc = A message deleted there.
    .noun = { $count ->
        [one] delete op
       *[other] delete ops
    }

d-my7lhn6o = Op

d-n7nishc3 = Channel

d-zfywcqnu = Webhook

d-2x53zyki = Message

d-cus46lo5 = Thread

d-5gtdjtwj = React op
    .doc = The bot's reaction added there, or taken back.
    .noun = { $count ->
        [one] react op
       *[other] react ops
    }

d-r5g7xdi4 = Op

d-5dlxmopv = Channel

d-uemzkzm4 = Message

d-adfgu63t = Emoji

d-3go4d34w = Start thread op
    .doc = A thread started on a message.
    .noun = { $count ->
        [one] start thread op
       *[other] start thread ops
    }

d-2dapcqci = Op

d-zza77yhy = Channel

d-qaa2bz2k = Message

d-3s3o7apm = Name

d-ycv24uiq = Tilo

d-bz2hkqhk = Create webhook op
    .doc = A channel's webhook made there.
    .noun = { $count ->
        [one] create webhook op
       *[other] create webhook ops
    }

d-6yzmzenq = Op

d-2dljfnsn = Channel

d-2whtwtgs = Tilo

d-v5fg4ke3 = Channel spec
    .doc = A channel made there, or brought back in line.
    .noun = { $count ->
        [one] channel spec
       *[other] channel specs
    }

d-ffcekihx = Op

d-obrvgv5z = Id

d-rdqrtii5 = Tilo

d-qt3bbw6a = Kind

d-zgdwmpjl = Name

d-fimljcem = Topic

d-ziohorjx = Parent

d-m2wvvjt3 = Position

d-v5fg4ke3-filter-kind = Kind

d-v5fg4ke3-filter-position = Position

d-4nww5mq7 = Delete channel op
    .doc = A channel deleted there.
    .noun = { $count ->
        [one] delete channel op
       *[other] delete channel ops
    }

d-sys4gzlz = Op

d-wvug6ece = Channel

d-6m5og6gi = Category place
    .doc = Where a category goes.
    .noun = { $count ->
        [one] category place
       *[other] category places
    }

d-admu5bl6 = Category

d-lnbqfx6g = Position

d-6m5og6gi-filter-position = Position

d-zblqsbjd = Channel place
    .doc = Where a channel goes: under `parent` (empty for the top level), at `position`.
    .noun = { $count ->
        [one] channel place
       *[other] channel places
    }

d-5ngwergg = Channel

d-goi56ega = Parent

d-6ueqeq32 = Position

d-zblqsbjd-filter-position = Position

d-dzmowlta = Arrange op
    .doc = Channels and categories put in their places at once.
    .noun = { $count ->
        [one] arrange op
       *[other] arrange ops
    }

d-kydu2gaa = Op

d-6jnxth7g = Categories

d-xn4ydnwp = Channels

d-iscjffcy = Role spec
    .doc = A role made there, or brought back in line.
    .noun = { $count ->
        [one] role spec
       *[other] role specs
    }

d-ltm6qttx = Op

d-vj4acred = Id

d-wkspbvxi = Tilo

d-kseg5wme = Name

d-q5b5zn6p = Permissions

d-fm2qokk6 = Color

d-iscjffcy-filter-color = Color

d-iuhrdwc7 = Delete role op
    .doc = A role deleted there.
    .noun = { $count ->
        [one] delete role op
       *[other] delete role ops
    }

d-nj3ocrig = Op

d-hsrydeyz = Role

d-am4zw3ol = Member roles op
    .doc = A linked member's roles set there.
    .noun = { $count ->
        [one] member roles op
       *[other] member roles ops
    }

d-jrpl4evb = Op

d-dxd7g4ng = User

d-epfankx7 = Add

d-mvj6iqwh = Remove

d-qtvddjlx = Member op
    .doc = A linked member kicked, banned or unbanned there.
    .noun = { $count ->
        [one] member op
       *[other] member ops
    }

d-3kdyizst = Op

d-hnhr6dsd = User

d-3oghfcwy = Reason

d-7odxiitw = Overwrite op
    .doc = A channel's permission overwrite set there.
    .noun = { $count ->
        [one] overwrite op
       *[other] overwrite ops
    }

d-kwsytqig = Op

d-syrsf566 = Channel

d-fohcx76y = Target

d-v7ggg7lk = Target kind
    .doc = 0 for a role, 1 for a member.

d-x6idw22j = Allow

d-rakigbnk = Deny

d-7odxiitw-filter-target_kind = Target kind

d-s25bqwlq = Fetch asset op
    .doc = A file copied from Discord's CDN into the workspace's content store.
    .noun = { $count ->
        [one] fetch asset op
       *[other] fetch asset ops
    }

d-5w4mqrie = Op

d-77p2naze = Url

d-cqzm3ho2 = Max bytes

d-s25bqwlq-filter-max_bytes = Max bytes

d-2tqwhmeu = Answer interaction op
    .doc = A slash command's reply edited to what came of it.
    .noun = { $count ->
        [one] answer interaction op
       *[other] answer interaction ops
    }

d-rbrxnuru = Op

d-npc2czod = Interaction

d-isol4ubx = Content

d-2wngdhgu = Emoji spec
    .doc = A workspace emoji uploaded to the server, or renamed there.
    .noun = { $count ->
        [one] emoji spec
       *[other] emoji specs
    }

d-fq24nqqf = Op

d-akfyjbod = Id

d-mzfkhklg = Name

d-wol52pvk = Picture

d-eohdpyuf = Tilo

d-la7azwt7 = Delete emoji op
    .doc = An emoji the bridge made deleted there.
    .noun = { $count ->
        [one] delete emoji op
       *[other] delete emoji ops
    }

d-c644sdba = Op

d-q6rbppuu = Emoji

d-qdnbgv6d = Tilo

d-2hufa3gt = Perform
    .doc = What the bridge carries out: which account's server, and the call, as JSON.
    .noun = { $count ->
        [one] perform
       *[other] performs
    }

d-l4rpwufl = Account

d-4billcrq = Op

d-tmtkkfia = Carried
    .doc = A conversation the bridge carries: its channel there, the channel's webhook, and the thread when the conversation is one.
    .noun = { $count ->
        [one] carried
       *[other] carrieds
    }

d-b3z6vpk2 = Channel

d-qd3pgrbw = Webhook

d-iz2m6xla = Thread

d-3mu7ywdt = There
    .doc = Who a message here is on Discord: the account the person linked, else the workspace's name for them.
    .noun = { $count ->
        [one] there
       *[other] theres
    }

d-aau5m6m5 = Name

d-u2mszhyt = Avatar

d-s5v6kdoq = Start
    .doc = What connecting the person's account offers: where to sign in, and the digest of the state that binds what comes back to them.
    .noun = { $count ->
        [one] start
       *[other] starts
    }

d-w2nhoqh5 = Url

d-gotwuzdw = Sha256

d-rnrzcit5 = Sign in ask
    .doc = What starting a sign-in asks: where Discord's consent page is, without its state.
    .noun = { $count ->
        [one] sign in ask
       *[other] sign in asks
    }

d-tgpwyfis = Page

d-kj2khaip = Voice pair
    .doc = A Discord voice channel and the voice channel here it is carried to: one call, heard on both sides.
    .noun = { $count ->
        [one] voice pair
       *[other] voice pairs
    }

d-h26if7bj = Discord

d-kar7woty = Tilo

d-az5m6jmq = Ownership
    .doc = Whether the server is still being brought in, or bridged both ways.

d-utrsfg4x = Importing

d-y24lg4rn = Adopted

d-h2wzefo7 = Mapped kind
    .doc = What a mapping joins.

d-3gbztc2q = Channel

d-73givnh3 = Category

d-s44bxrw2 = Thread

d-yzyti2ix = Message

d-zrltiujh = Webhook

d-trh4pulm = Role

d-rphauowe = Emoji

d-njpjnwo2 = Origin side
    .doc = Which side a mapped object was made on.

d-d3uwpxoi = Tilo

d-moeajhpd = Discord

d-so6xwfff = Channel kind
    .doc = What kind of place a Discord channel is.

d-gai3jxbz = Text

d-dygnzgoc = Voice

d-p4gznreo = Category

d-plvd6wd3 = Thread

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

d-peb7lz2j-turn = Turn

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

d-g6uruw6c = Connect Discord
    .doc = Starts the account: the record the setup writes into.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server

d-m66js2te = Connect Discord
    .doc = Opens the setup dialog.

d-5gixvrmc = Closes it
    .doc = Closes it.

d-c75oubac = Clear problem
    .doc = Forgets what the setup last said went wrong, as choosing a channel does.

d-egu22f56 = Bring it in
    .doc = Binds this account to one server, with the token its bot uses.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-the-bot-s-token-is-needed = The bot's token is needed
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-egu22f56-s = S

d-egu22f56-token = Token

d-mbfpbi55 = Find
    .doc = Finds the servers the bot is in, so one can be chosen.
    .w-only-the-workspace-s-moderators-find = Only the workspace's moderators find the bot's servers
    .w-the-bot-s-token-is-needed = The bot's token is needed

d-mbfpbi55-token = Token

d-use3abdi = Connect
    .doc = Binds this account to one server.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-use3abdi-guild_id = Guild id

d-use3abdi-name = Name

d-use3abdi-only_channel = Only channel

d-use3abdi-mirror_channels = Mirror channels

d-use3abdi-map_roles = Map roles

d-y7ozxfz7 = Observe layout
    .doc = Records Discord's layout as the latest event reported it: channels and roles the layout no longer lists go, the ones it lists are brought up to date, and new ones come in.
    .w-only-the-bridge-keeps-the-server = Only the bridge keeps the server's layout

d-y7ozxfz7-bot_user = Bot user

d-y7ozxfz7-channels = Channels

d-y7ozxfz7-roles = Roles

d-y7ozxfz7-emojis = Emojis

d-4sfnlxls = Map
    .doc = Joins a conversation, message or the like here to its Discord side.
    .w-only-the-bridge-joins-the-two = Only the bridge joins the two sides
    .w-a-mapping-joins-two-ids = A mapping joins two ids

d-4sfnlxls-kind = Kind

d-4sfnlxls-tilo = Tilo

d-4sfnlxls-discord = Discord

d-4sfnlxls-origin = Origin

d-4sfnlxls-persona = Persona

d-4sfnlxls-root = Root

d-chku3gmm = Unmap
    .doc = Forgets a mapping, keeping the two sides apart from now on when asked.
    .w-only-the-bridge-joins-the-two = Only the bridge joins the two sides

d-chku3gmm-kind = Kind

d-chku3gmm-tilo = Tilo

d-chku3gmm-keep_apart = Keep apart

d-zjhuwnfn = Carry message
    .doc = Carries a message written here to its conversation there.
    .w-the-conversation-is-not-bridged = The conversation is not bridged

d-zjhuwnfn-c = C

d-zjhuwnfn-m = M

d-5s33fint = Carry edit
    .doc = Carries a change to a message written here.

d-5s33fint-c = C

d-5s33fint-m = M

d-5s33fint-there = There

d-xkr2eff4 = Carry delete
    .doc = Carries a message's deletion.

d-xkr2eff4-c = C

d-xkr2eff4-there = There

d-xkr2eff4-webhook = Webhook

d-66ourqe5 = Carry reaction
    .doc = Puts the bot's reaction on a message there, or takes it back.

d-66ourqe5-c = C

d-66ourqe5-there = There

d-66ourqe5-emoji = Emoji

d-66ourqe5-add = Add

d-3bcjppqv = Begin connect
    .doc = Starts linking the person's Discord account: the consent page to open, with this person's state, which the account keeps hashed.
    .w-this-workspace-is-not-bridged-to = This workspace is not bridged to a Discord server
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-3bcjppqv-state_sha256 = State sha256

d-scoyir6n = Connect
    .doc = Starts signing in with Discord, as the person looking: answers where to go.
    .w-this-workspace-is-not-bridged-to = This workspace is not bridged to a Discord server
    .w-this-device-does-not-offer-signing = This device does not offer signing in with Discord

d-bgsbczd2 = Offer link
    .doc = Offers a Discord account for linking, from the server: `/link` answered with a one-time token, whose check this is.
    .w-only-the-bridge-offers-a-link = Only the bridge offers a link
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-bgsbczd2-check = Check

d-bgsbczd2-discord_user = Discord user

d-bgsbczd2-user_name = User name

d-bgsbczd2-display_name = Display name

d-bgsbczd2-avatar = Avatar

d-bgsbczd2-interaction = Interaction

d-ufl2i6na = Offer sign in
    .doc = Offers the account that signed in on Discord's consent page, to the person who started the round trip `state_sha256` names.
    .w-only-the-bridge-offers-a-link = Only the bridge offers a link
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits
    .w-that-sign-in-with-discord-was = That sign-in with Discord was not started here, or took too long

d-ufl2i6na-check = Check

d-ufl2i6na-discord_user = Discord user

d-ufl2i6na-user_name = User name

d-ufl2i6na-state_sha256 = State sha256

d-ufl2i6na-display_name = Display name

d-ufl2i6na-avatar = Avatar

d-ed2q354a = Keep offer
    .doc = Keeps an offer for ten minutes, dropping those run out.
    .w-only-the-bridge-keeps-an-offer = Only the bridge keeps an offer

d-ed2q354a-offered = Offered

d-d27ryfcg = Complete link
    .doc = Links the Discord account a one-time token offers to the person looking.
    .w-this-link-was-not-started-here = This link was not started here
    .w-this-link-was-already-used-or = This link was already used or has run out. Start again from Discord with /link
    .w-this-link-ran-out-after-ten = This link ran out after ten minutes. Start again with /link
    .w-this-link-was-started-by-another = This link was started by another account. Sign in as that account, or start again

d-d27ryfcg-proof = Proof

d-de57mxka = Set sign in
    .doc = Offers signing in with Discord: this device's consent page, without its state; empty for none.
    .w-only-the-workspace-s-moderators-set = Only the workspace's moderators set up signing in with Discord
    .w-a-consent-page-is-an-http = A consent page is an http(s) address with its query

d-de57mxka-page = Page

d-zkbkstyz = Answer interaction
    .doc = Edits a slash command's reply to what came of it.
    .w-only-the-bridge-answers-a-slash = Only the bridge answers a slash command
    .w-a-reply-holds-1-to-2000 = A reply holds 1 to 2000 characters

d-zkbkstyz-op = Op

d-3pkifqcd = Rename link
    .doc = Takes a linked account's newer names and avatar.
    .w-only-the-bridge-takes-new-names = Only the bridge takes new names for a link

d-3pkifqcd-discord_user = Discord user

d-3pkifqcd-user_name = User name

d-3pkifqcd-display_name = Display name

d-3pkifqcd-avatar = Avatar

d-zzxkntte = Set link photo
    .doc = Keeps a linked account's avatar as the person's picture, until they upload their own.
    .w-only-the-bridge-keeps-a-link = Only the bridge keeps a link's picture
    .w-a-picture-is-a-base64-data = A picture is a base64 data: URL of a PNG, JPEG or WebP

d-zzxkntte-discord_user = Discord user

d-zzxkntte-avatar = Avatar

d-zzxkntte-photo = Photo

d-mryx6vao = Unlink
    .doc = Unlinks a Discord account: its person's, or the bridge's.
    .w-only-the-workspace-s-moderators-or = Only the workspace's moderators, or the person it is with, unlink a Discord account

d-mryx6vao-discord_user = Discord user

d-3a6xorbm = Unlink
    .doc = Unlinks the person's own Discord account.

d-xh3uub4v = Block persona
    .doc = Stops mirroring a Discord account: its messages are no longer carried, and the ones already here are removed.
    .w-only-the-workspace-s-moderators-block = Only the workspace's moderators block a Discord account

d-xh3uub4v-discord_user = Discord user

d-kpop6fsx = Unblock persona
    .doc = Mirrors a Discord account again.
    .w-only-the-workspace-s-moderators-block = Only the workspace's moderators block a Discord account

d-kpop6fsx-discord_user = Discord user

d-byghr2ra = Remember avatar
    .doc = Remember where an avatar's image is stored.
    .w-only-the-bridge-keeps-an-avatar = Only the bridge keeps an avatar
    .w-an-avatar-hash-and-a-content = An avatar hash and a content address

d-byghr2ra-hash = Hash

d-byghr2ra-blob = Blob

d-essmh36e = Finish import
    .doc = Finishes adopting the server: everything in it is here now.
    .w-only-the-bridge-finishes-the-import = Only the bridge finishes the import
    .w-the-server-was-already-adopted = The server was already adopted

d-essmh36e-tilo_only = Tilo only

d-wzrzlz7b = Send there
    .doc = Sends a message through a channel's webhook, as the bridge plans it.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters
    .w-discord-takes-at-most-ten-files = Discord takes at most ten files of 10 MiB from a webhook

d-wzrzlz7b-op = Op

d-j7qdo46k = Edit there
    .doc = Edits a message the bridge sent.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters

d-j7qdo46k-op = Op

d-44cdrnzl = Delete there
    .doc = Deletes a message there.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks

d-44cdrnzl-op = Op

d-kd5j3wgh = React there
    .doc = Adds the bot's reaction.
    .w-the-bridge-carries-reactions-to-discord = The bridge carries reactions to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-kd5j3wgh-op = Op

d-wsbnmekd = Unreact there
    .doc = Takes the bot's reaction back.
    .w-the-bridge-carries-reactions-to-discord = The bridge carries reactions to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-wsbnmekd-op = Op

d-wpalyq3g = Start thread there
    .doc = Starts a thread on a message.
    .w-the-bridge-starts-threads-on-discord = The bridge starts threads on Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-wpalyq3g-op = Op

d-7syid47w = Create webhook
    .doc = Creates a channel's webhook.
    .w-the-bridge-makes-the-server-s = The bridge makes the server's webhooks
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-7syid47w-op = Op

d-err4lpx6 = Apply channel
    .doc = Creates a channel there, or brings it back in line.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-a-discord-channel-name-is-1 = A Discord channel name is 1 to 100 characters

d-err4lpx6-op = Op

d-wbopo5dk = Delete channel there
    .doc = Deletes a channel there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-wbopo5dk-op = Op

d-pkz2nl2v = Arrange channels
    .doc = Puts channels and categories in their places there, so the server lists them in this order.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-moves-only-the-channels = The bridge moves only the channels it bridges to another category

d-pkz2nl2v-op = Op

d-4myfgv7e = May mirror
    .doc = Says whether the bridge may change the server's channels: mirroring is on and the server is adopted.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first

d-f2qiijmn = May change
    .doc = Says whether the bridge made the Discord channel or category `id` for one here.
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-f2qiijmn-id = Id

d-mjxp37d6 = Apply role
    .doc = Creates a role there, or brings it back in line.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-the-bridge-changes-only-the-roles = The bridge changes only the roles it made

d-mjxp37d6-op = Op

d-z6gujdd5 = Delete role
    .doc = Deletes a role there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-the-bridge-deletes-only-roles-it = The bridge deletes only roles it manages

d-z6gujdd5-op = Op

d-jnvra2vz = Set member roles
    .doc = Sets a linked member's roles there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here
    .w-the-bridge-assigns-only-roles-it = The bridge assigns only roles it manages

d-jnvra2vz-op = Op

d-ghq2co4f = Kick
    .doc = Kicks a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-ghq2co4f-op = Op

d-wtn45vy2 = Ban
    .doc = Bans a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-wtn45vy2-op = Op

d-6f6uz657 = Unban
    .doc = Unbans a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-6f6uz657-op = Op

d-tqd5yd2e = Set overwrite
    .doc = Sets a channel's permission overwrite, for a role the bridge made.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's permissions
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-sets-overwrites-only-for = The bridge sets overwrites only for the roles it made

d-tqd5yd2e-op = Op

d-4nraemj7 = Fetch asset
    .doc = Copies a file from Discord's CDN into the workspace's content store.
    .w-the-bridge-copies-discord-s-files = The bridge copies Discord's files
    .w-chat-holds-files-of-at-most = Chat holds files of at most 32 MiB

d-4nraemj7-op = Op

d-us3vrzdv = Apply emoji
    .doc = Uploads a workspace emoji to the server, or renames one the bridge uploaded.
    .w-a-discord-emoji-s-name-is = A Discord emoji's name is 2 to 32 letters, digits and underscores
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded
    .w-an-emoji-op-names-its-picture = An emoji op names its picture's content address

d-us3vrzdv-op = Op

d-fmf7g3bm = Delete emoji
    .doc = Deletes an emoji the bridge uploaded.
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded

d-fmf7g3bm-op = Op

d-h5ch4f5f = Settle emoji
    .doc = Records what became of an emoji upload, rename or delete.
    .w-only-the-bridge-settles-an-emoji = Only the bridge settles an emoji
    .w-an-emoji-problem-is-1-to = An emoji problem is 1 to 300 characters

d-h5ch4f5f-tilo = Tilo

d-h5ch4f5f-emoji_id = Emoji id

d-h5ch4f5f-problem = Problem

d-h5ch4f5f-picture = Picture

d-h5ch4f5f-was = Was

d-h5ch4f5f-deleted = Deleted

d-i6b4mjgp = Report voice
    .doc = Says why the bridge cannot carry voice now, or that it can again.
    .w-only-the-bridge-reports-what-discord = Only the bridge reports what Discord said of a call
    .w-a-voice-problem-is-1-to = A voice problem is 1 to 300 characters

d-i6b4mjgp-problem = Problem

d-lwc7n7qs = Pump
    .doc = A value nothing reads, for the trigger's frame that does nothing itself: the origin polls Discord's events and the conversations here in its own loop.

d-2yeh74ot = New skill~present

d-ujq3oyls = Correct skill~present

d-hpamj3eq = Keep example~present

d-amkgebye = Report problem~present

d-pszgxdax = Message person~present

d-dgrvpxre = Send view~present

d-psjj65el = Relay~present

d-zmf3ccig = Relay reaction~present

d-mdviqjz2 = Delete channel~present

d-czomdsi6 = Delete category~present

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
    .w-workspace = Workspace
    .w-workspace-skills = Workspace skills
    .w-discord = Discord
    .w-no-channel-open = No channel open

d-ymtess4x = Channel
    .w-message = Message #{ $name }
    .w-send = Send
    .w-wrote-this-in = { $by } wrote this in { $written_in }
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit

d-jbezqvlc = Direct
    .w-message = Message { $name }
    .w-send = Send
    .w-wrote-this-in = { $by } wrote this in { $written_in }
    .w-pinned = Pinned
    .w-edit-message = Edit message
    .w-cancel = Cancel
    .w-save = Save
    .w-reply = Reply
    .w-edit = Edit

d-q45utnmh = Channel summary

d-qnkkcusk = Voice

d-lkjppscz = Voice summary

d-enkldjck = Direct summary

d-oqg63mfd = Category summary

d-hrbsxhor = Skill row

d-enkgjyn7 = Thread
    .w-message = Message { $name }
    .w-send = Send
    .w-wrote-this-in = { $by } wrote this in { $written_in }
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

d-tfqsx7pw = Discord
    .w-bring-a-discord-server-into-this = Bring a Discord server into this workspace
    .w-its-channels-become-conversations-here-and = Its channels become conversations here, and what people say on either side shows on both.
    .w-connect-discord = Connect Discord

d-jd4rwhpj = Begin setup~present

d-3oavqe5x = Account
    .w-bring-a-discord-server-into-this = Bring a Discord server into this workspace
    .w-its-channels-become-conversations-here-and = Its channels become conversations here, and what people say on either side shows on both.
    .w-connect-discord = Connect Discord
    .w-discord = Discord
    .w-people = People
    .w-being-brought-in = Being brought in
    .w-bridged = Bridged
    .w-what-people-say-on-either-side = What people say on either side shows on both.
    .w-calls = Calls
    .w-emoji = Emoji
    .w-you = You: { $shown_name }
    .w-what-you-say-on-discord-shows = What you say on Discord shows as you.
    .w-unlink-your-discord-account = Unlink your Discord account
    .w-link-your-discord-account = Link your Discord account
    .w-what-you-say-there-then-shows = What you say there then shows as you.
    .w-link-your-discord-account-from-the = Link your Discord account from the server with /link
    .w-linked = Linked
    .w-the-server = The server

d-blbvk7ew = Setup dialog
    .w-connect-a-discord-server = Connect a Discord server
    .w-cancel = Cancel
    .w-choose-the-server-to-bring-in = Choose the server to bring in.
    .w-the-bot-s-token-is-kept = The bot's token is kept on this device, never in the workspace's records.
    .w-the-bot-s-token = The bot's token
    .w-find-the-bot-s-servers = Find the bot's servers

d-c5ro4qhq = Bridge server~present

d-ijedhuk4 = Find servers~present

d-fzzrx4jf = Connect~present

d-4dfzk5jr = Observe layout~present

d-4jv2klww = Map~present

d-njamwmvc = Unmap~present

d-whn6midu = Carry message~present

d-fxjubiox = Begin connect~present

d-ev42yozz = Connect account~present

d-krxrmgz2 = Offer link~present

d-o3zzn75h = Offer sign in~present

d-te7d5m64 = Keep offer~present

d-3ibs4nq2 = Complete link~present

d-rsnjjlgv = Set sign in~present

d-e6upzw2n = Answer interaction~present

d-ls2pn5s2 = Rename link~present

d-2nof7l5v = Set link photo~present

d-nksrqevn = Unlink~present

d-qtjedbse = Block persona~present

d-rv5rlksw = Unblock persona~present

d-v27diibh = Remember avatar~present

d-ml7d57cm = Finish import~present

d-h5oenuw3 = Send there~present

d-3ue3nvmz = Edit there~present

d-hwxt7chz = Delete there~present

d-z2pymxb6 = React there~present

d-v6ek3fsb = Unreact there~present

d-b3tmeuy5 = Start thread there~present

d-5mksnzxp = Create webhook~present

d-5tjib7ms = Apply channel~present

d-kqmmvqpp = Delete channel there~present

d-2awz54fs = Arrange channels~present

d-xch2jsdj = May mirror~present

d-w4ybx7ty = May change~present

d-rjraqpaz = Apply role~present

d-cmaajofc = Delete role~present

d-lbyvgtl5 = Set member roles~present

d-ixetp6ly = Kick~present

d-ld4jgbmk = Ban~present

d-6lxgicg3 = Unban~present

d-fthix5f6 = Set overwrite~present

d-l5ohypbr = Fetch asset~present

d-o5lutbs4 = Apply emoji~present

d-mu6xjlrj = Delete emoji~present

d-qeypjshq = Settle emoji~present

d-wc43y6ta = Report voice~present
