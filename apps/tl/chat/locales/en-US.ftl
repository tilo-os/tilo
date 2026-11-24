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

d-icam2xtc = New channel
    .doc = Adds a text channel to the sidebar, under `category` when it is given and about `topic` when that is.
    .w-new-channel = new-channel

d-icam2xtc-name = Name

d-icam2xtc-topic = Topic

d-icam2xtc-category = Category

d-l6x6gpo3 = New category
    .doc = Adds a category to the sidebar.
    .w-new-category = New category

d-l6x6gpo3-name = Name

d-gxmgzgtd = New voice channel
    .doc = Adds a voice channel to the sidebar, under `category` when it is given.
    .w-voice = Voice

d-gxmgzgtd-name = Name

d-gxmgzgtd-category = Category

d-nsalxxbl = New skill
    .doc = Starts a skill, called `name`.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-nsalxxbl-name = Name

d-h3hkwpay = Correct skill
    .doc = Corrects a skill: only what is given changes, and what is left out stays as it is.
    .w-the-workspace-already-has-a-skill = The workspace already has a skill called { $name }

d-h3hkwpay-name = Name

d-h3hkwpay-work = Work

d-h3hkwpay-how = How

d-h3hkwpay-form = Form

d-h3hkwpay-model = Model

d-ulrgs22m = Forget skill
    .doc = Forgets a skill: no later turn reads it.

d-3c5xkf42 = Keep an example
    .doc = Keeps an example of a skill: a result the person accepted (more like this) or rejected (less like this), with what they said about it.
    .w-an-example-says-what-the-result = An example says what the result was

d-3c5xkf42-text = Text

d-3c5xkf42-accepted = Accepted

d-3c5xkf42-note = Note

d-yrqkjn2w = Remove the example
    .doc = Takes an example out of a skill.

d-yrqkjn2w-example = Example

d-przcjkgz = Carry direct people
    .doc = The direct conversations an earlier version kept with one person alone (`person`), which every member could read: in each are that person and whoever opened it, as its history says.

d-c5rprye7 = Carry native chat
    .doc = The conversations the native Chat kept before this app took its id over, carried into these once: its categories and channels in the sidebar's order, each with its name and topic, and every message of a channel with what it said, whether it was pinned and the id it had there.

d-smdxwudx = Carry native skills
    .doc = The workspace's skills the native Chat kept before this app took its id over, carried into these once: what each is called, the work it is for, how the work is done here, the form of its result and the model, under the id it had there and in the order it stood, and the examples it kept, oldest first, each with what the result was, whether the person accepted it, what they said and when it was kept.

d-hihjyxjx = Report a problem
    .doc = Files a problem report, what someone ran into while using Tilo, in the workspace's #feedback channel, which the first report opens.
    .did = reported a problem
    .w-say-what-went-wrong-first = Say what went wrong first

d-hihjyxjx-body = Body

d-cq2wzjze = Message
    .doc = Opens the direct message between the invoker and `who`, in the sidebar, or the one already open between the two of them: two people have one conversation, not a new empty one each time.
    .w-is-not-in-this-workspace = { $name } is not in this workspace

d-cq2wzjze-who = Who

d-h3fas3fa = Conversation with
    .doc = The direct conversation between the invoker and `who`, none while the two of them have none: what an agent, a script or another app reads to find it, opening and making nothing.

d-h3fas3fa-who = Who

d-bsk6d7ep = Unread
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

d-mbs3hcxg = Write to
    .doc = Writes `body` to `who`, a person or an agent, in the one direct conversation with them: the one already open, else a new one in the sidebar.

d-mbs3hcxg-who = Who

d-mbs3hcxg-body = Body

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

d-7oymhczp = Relay a reaction
    .doc = Relays a reaction someone gave a message where it was written, as that place names them: the counterpart of `relay` for a reaction.
    .did = relayed a reaction
    .w-say-which-emoji = Say which emoji
    .w-say-who-gave-the-reaction = Say who gave the reaction
    .w-say-what-they-are-called-there = Say what they are called there

d-7oymhczp-emoji = Emoji

d-7oymhczp-persona = Persona

d-7oymhczp-by = By

d-7oymhczp-given = Given

d-jozxp5qy = Delete channel
    .doc = Deletes a channel and what was said in it, from the sidebar or the category it is in.
    .w-only-the-workspace-s-moderators-delete = Only the workspace's moderators delete a channel

d-jozxp5qy-from = From

d-jozxp5qy-ch = Ch

d-samyx2wv = Delete category
    .doc = Deletes a category that holds no channel.
    .w-only-the-workspace-s-moderators-delete = Only the workspace's moderators delete a category
    .w-move-or-delete-its-channels-first = Move or delete its channels first

d-samyx2wv-c = C

d-irofwg3m = Move channel
    .doc = Moves a channel to a place in the sidebar: under `category`, or outside any, at `at` among the channels listed there (the end when it is negative).

d-irofwg3m-ch = Ch

d-irofwg3m-category = Category

d-irofwg3m-at = At

d-gsjd4ujv = Move category
    .doc = Moves a category to a place in the sidebar, at `at` among the categories and channels listed there (the end when it is negative).

d-gsjd4ujv-c = C

d-gsjd4ujv-at = At

d-rwtwveul = Edit
    .doc = Changes what a message says.
    .w-only-its-author-can-edit-a = Only its author can edit a message
    .w-type-a-message-first = Type a message first

d-rwtwveul-body = Body

d-rwtwveul-turn = Turn

d-vhmhrq2t = Delete
    .doc = Deletes a message from the channel or thread it is in.

d-vhmhrq2t-to = To

d-vhmhrq2t-m = M

d-y3ydowwa = Pin
    .doc = Pins a message to its channel, or unpins it.
    .w-pin = Pin
    .w-unpin = Unpin

d-y3ydowwa-pinned = Pinned

d-udbzedv6 = React
    .doc = Adds your reaction with an emoji to a message, or takes it back when you had reacted with it.

d-udbzedv6-emoji = Emoji

d-7sjai347 = Report
    .doc = Reports a message of a conversation to the workspace's moderators, saying why when the reporter does.
    .did = reported a message

d-7sjai347-to = To

d-7sjai347-m = M

d-7sjai347-reason = Reason

d-v6ok6hod = Decide
    .doc = Settles a report: what a moderator decided about it.
    .w-only-the-workspace-s-moderators-settle = Only the workspace's moderators settle a report
    .w-say-what-was-decided = Say what was decided

d-v6ok6hod-decision = Decision

d-6uysx57s = Start thread
    .doc = Starts a thread from a message of a channel or a direct conversation, named after what the message says.

d-6uysx57s-c = C

d-6uysx57s-m = M

d-6uysx57s-name = Name

d-wtn2x6nk = Connect Discord
    .doc = Starts the account: the record the setup writes into.

d-aqo2wily = Connect Discord
    .doc = Opens the setup dialog.

d-q7mjkhr4 = Closes it
    .doc = Closes it.

d-fku77yca = Clear problem
    .doc = Forgets what the setup last said went wrong, as choosing a channel does.

d-qr33zcoc = Bring it in
    .doc = Binds this account to one server, with the token its bot uses.
    .w-the-bot-s-token-is-needed = The bot's token is needed
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-qr33zcoc-s = S

d-qr33zcoc-token = Token

d-clziyxuy = Find
    .doc = Finds the servers the bot is in, so one can be chosen.
    .w-the-bot-s-token-is-needed = The bot's token is needed

d-clziyxuy-token = Token

d-tbnre46z = Connect
    .doc = Binds this account to one server.
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-tbnre46z-guild_id = Guild id

d-tbnre46z-name = Name

d-tbnre46z-only_channel = Only channel

d-tbnre46z-mirror_channels = Mirror channels

d-tbnre46z-map_roles = Map roles

d-xjuak3vz = Observe layout
    .doc = Records Discord's layout as the latest event reported it: channels and roles the layout no longer lists go, the ones it lists are brought up to date, and new ones come in.

d-xjuak3vz-bot_user = Bot user

d-xjuak3vz-channels = Channels

d-xjuak3vz-roles = Roles

d-xjuak3vz-emojis = Emojis

d-q7nxeqbj = Map
    .doc = Joins a conversation, message or the like here to its Discord side.
    .w-a-mapping-joins-two-ids = A mapping joins two ids

d-q7nxeqbj-kind = Kind

d-q7nxeqbj-tilo = Tilo

d-q7nxeqbj-discord = Discord

d-q7nxeqbj-origin = Origin

d-q7nxeqbj-persona = Persona

d-q7nxeqbj-root = Root

d-o5nqfd3z = Unmap
    .doc = Forgets a mapping, keeping the two sides apart from now on when asked.

d-o5nqfd3z-kind = Kind

d-o5nqfd3z-tilo = Tilo

d-o5nqfd3z-keep_apart = Keep apart

d-qjvxfstp = Carry message
    .doc = Carries a message written here to its conversation there.
    .w-the-conversation-is-not-bridged = The conversation is not bridged

d-qjvxfstp-c = C

d-qjvxfstp-m = M

d-akfluyiw = Carry edit
    .doc = Carries a change to a message written here.

d-akfluyiw-c = C

d-akfluyiw-m = M

d-akfluyiw-there = There

d-zjpwf3v3 = Carry delete
    .doc = Carries a message's deletion.

d-zjpwf3v3-c = C

d-zjpwf3v3-there = There

d-zjpwf3v3-webhook = Webhook

d-fxo37nlw = Carry reaction
    .doc = Puts the bot's reaction on a message there, or takes it back.

d-fxo37nlw-c = C

d-fxo37nlw-there = There

d-fxo37nlw-emoji = Emoji

d-fxo37nlw-add = Add

d-sclkcdde = Begin connect
    .doc = Starts linking the person's Discord account: the consent page to open, with this person's state, which the account keeps hashed.
    .w-this-workspace-is-not-bridged-to = This workspace is not bridged to a Discord server
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-sclkcdde-state_sha256 = State sha256

d-wthekoyh = Connect
    .doc = Starts signing in with Discord, as the person looking: answers where to go.
    .w-this-workspace-is-not-bridged-to = This workspace is not bridged to a Discord server
    .w-this-device-does-not-offer-signing = This device does not offer signing in with Discord

d-7wrzpvnu = Offer link
    .doc = Offers a Discord account for linking, from the server: `/link` answered with a one-time token, whose check this is.
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-7wrzpvnu-check = Check

d-7wrzpvnu-discord_user = Discord user

d-7wrzpvnu-user_name = User name

d-7wrzpvnu-display_name = Display name

d-7wrzpvnu-avatar = Avatar

d-7wrzpvnu-interaction = Interaction

d-kbhqns3n = Offer sign in
    .doc = Offers the account that signed in on Discord's consent page, to the person who started the round trip `state_sha256` names.
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits
    .w-that-sign-in-with-discord-was = That sign-in with Discord was not started here, or took too long

d-kbhqns3n-check = Check

d-kbhqns3n-discord_user = Discord user

d-kbhqns3n-user_name = User name

d-kbhqns3n-state_sha256 = State sha256

d-kbhqns3n-display_name = Display name

d-kbhqns3n-avatar = Avatar

d-sbk7zv2v = Keep offer
    .doc = Keeps an offer for ten minutes, dropping those run out.

d-sbk7zv2v-offered = Offered

d-m5oyg3vq = Complete link
    .doc = Links the Discord account a one-time token offers to the person looking.
    .w-this-link-was-not-started-here = This link was not started here
    .w-this-link-was-already-used-or = This link was already used or has run out. Start again from Discord with /link
    .w-this-link-ran-out-after-ten = This link ran out after ten minutes. Start again with /link
    .w-this-link-was-started-by-another = This link was started by another account. Sign in as that account, or start again

d-m5oyg3vq-proof = Proof

d-mwr7jfev = Set sign in
    .doc = Offers signing in with Discord: this device's consent page, without its state; empty for none.
    .w-a-consent-page-is-an-http = A consent page is an http(s) address with its query

d-mwr7jfev-page = Page

d-enk5xk2y = Answer interaction
    .doc = Edits a slash command's reply to what came of it.
    .w-a-reply-holds-1-to-2000 = A reply holds 1 to 2000 characters

d-enk5xk2y-op = Op

d-fpdm2ato = Rename link
    .doc = Takes a linked account's newer names and avatar.

d-fpdm2ato-discord_user = Discord user

d-fpdm2ato-user_name = User name

d-fpdm2ato-display_name = Display name

d-fpdm2ato-avatar = Avatar

d-7klrenaf = Set link photo
    .doc = Keeps a linked account's avatar as the person's picture, until they upload their own.
    .w-a-picture-is-a-base64-data = A picture is a base64 data: URL of a PNG, JPEG or WebP

d-7klrenaf-discord_user = Discord user

d-7klrenaf-avatar = Avatar

d-7klrenaf-photo = Photo

d-3gmghrak = Unlink
    .doc = Unlinks a Discord account: its person's, or the bridge's.

d-3gmghrak-discord_user = Discord user

d-xk3nfcw5 = Unlink
    .doc = Unlinks the person's own Discord account.

d-oqd5vcwe = Block persona
    .doc = Stops mirroring a Discord account: its messages are no longer carried, and the ones already here are removed.

d-oqd5vcwe-discord_user = Discord user

d-5r5qje3a = Unblock persona
    .doc = Mirrors a Discord account again.

d-5r5qje3a-discord_user = Discord user

d-5buphxzr = Remember avatar
    .doc = Remember where an avatar's image is stored.
    .w-an-avatar-hash-and-a-content = An avatar hash and a content address

d-5buphxzr-hash = Hash

d-5buphxzr-blob = Blob

d-f2yvfj6u = Finish import
    .doc = Finishes adopting the server: everything in it is here now.
    .w-the-server-was-already-adopted = The server was already adopted

d-f2yvfj6u-tilo_only = Tilo only

d-yhxktq35 = Send there
    .doc = Sends a message through a channel's webhook, as the bridge plans it.
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters
    .w-discord-takes-at-most-ten-files = Discord takes at most ten files of 10 MiB from a webhook

d-yhxktq35-op = Op

d-ji25spbq = Edit there
    .doc = Edits a message the bridge sent.
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters

d-ji25spbq-op = Op

d-pkqyglft = Delete there
    .doc = Deletes a message there.
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks

d-pkqyglft-op = Op

d-rmqw43jw = React there
    .doc = Adds the bot's reaction.
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-rmqw43jw-op = Op

d-tlsfrmec = Unreact there
    .doc = Takes the bot's reaction back.
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-tlsfrmec-op = Op

d-rsfz76jv = Start thread there
    .doc = Starts a thread on a message.
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-rsfz76jv-op = Op

d-ht42h7qo = Create webhook
    .doc = Creates a channel's webhook.
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-ht42h7qo-op = Op

d-l7gotjuk = Apply channel
    .doc = Creates a channel there, or brings it back in line.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-a-discord-channel-name-is-1 = A Discord channel name is 1 to 100 characters

d-l7gotjuk-op = Op

d-skglib6y = Delete channel there
    .doc = Deletes a channel there.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-skglib6y-op = Op

d-4lljwkzo = Arrange channels
    .doc = Puts channels and categories in their places there, so the server lists them in this order.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-moves-only-the-channels = The bridge moves only the channels it bridges to another category

d-4lljwkzo-op = Op

d-uhf7dif5 = May mirror
    .doc = Says whether the bridge may change the server's channels: mirroring is on and the server is adopted.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first

d-7j4quo42 = May change
    .doc = Says whether the bridge made the Discord channel or category `id` for one here.
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-7j4quo42-id = Id

d-72fx4vut = Apply role
    .doc = Creates a role there, or brings it back in line.
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-the-bridge-changes-only-the-roles = The bridge changes only the roles it made

d-72fx4vut-op = Op

d-xx22vzgm = Delete role
    .doc = Deletes a role there.
    .w-the-bridge-deletes-only-roles-it = The bridge deletes only roles it manages

d-xx22vzgm-op = Op

d-kcgcjzxp = Set member roles
    .doc = Sets a linked member's roles there.
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here
    .w-the-bridge-assigns-only-roles-it = The bridge assigns only roles it manages

d-kcgcjzxp-op = Op

d-5kjmv2y6 = Kick
    .doc = Kicks a linked member there.
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-5kjmv2y6-op = Op

d-qzj2skmh = Ban
    .doc = Bans a linked member there.
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-qzj2skmh-op = Op

d-ebnyww6y = Unban
    .doc = Unbans a linked member there.
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-ebnyww6y-op = Op

d-bcv3bpdd = Set overwrite
    .doc = Sets a channel's permission overwrite, for a role the bridge made.
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-sets-overwrites-only-for = The bridge sets overwrites only for the roles it made

d-bcv3bpdd-op = Op

d-4746fhdj = Fetch asset
    .doc = Copies a file from Discord's CDN into the workspace's content store.
    .w-chat-holds-files-of-at-most = Chat holds files of at most 32 MiB

d-4746fhdj-op = Op

d-pxxjglwl = Apply emoji
    .doc = Uploads a workspace emoji to the server, or renames one the bridge uploaded.
    .w-a-discord-emoji-s-name-is = A Discord emoji's name is 2 to 32 letters, digits and underscores
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded
    .w-an-emoji-op-names-its-picture = An emoji op names its picture's content address

d-pxxjglwl-op = Op

d-jwqnvbkg = Delete emoji
    .doc = Deletes an emoji the bridge uploaded.
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded

d-jwqnvbkg-op = Op

d-2tmf3r7t = Settle emoji
    .doc = Records what became of an emoji upload, rename or delete.
    .w-an-emoji-problem-is-1-to = An emoji problem is 1 to 300 characters

d-2tmf3r7t-tilo = Tilo

d-2tmf3r7t-emoji_id = Emoji id

d-2tmf3r7t-problem = Problem

d-2tmf3r7t-picture = Picture

d-2tmf3r7t-was = Was

d-2tmf3r7t-deleted = Deleted

d-gtxxbc2x = Report voice
    .doc = Says why the bridge cannot carry voice now, or that it can again.
    .w-a-voice-problem-is-1-to = A voice problem is 1 to 300 characters

d-gtxxbc2x-problem = Problem

d-rgx4lzoa = Pump
    .doc = A value nothing reads, for the trigger's frame that does nothing itself: the origin polls Discord's events and the conversations here in its own loop.

d-e4foaopg = New skill~present

d-yqycvdlg = Correct skill~present

d-2kjmfhwr = Keep example~present

d-vpo3nysh = Report problem~present

d-3ascfbc2 = Message person~present

d-dgrvpxre = Send view~present

d-psjj65el = Relay~present

d-mtc7hgou = Relay reaction~present

d-mdviqjz2 = Delete channel~present

d-zo7qhq3y = Delete category~present

d-ivhvfgyu = Edit~present

d-oqpfpkj2 = Pin~present

d-57izvt2o = Decide~present

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

d-couomik6 = Bridge server~present

d-itifxsde = Find servers~present

d-ibf2m6os = Connect~present

d-jim24hpi = Map~present

d-4qxfrldo = Carry message~present

d-n62frfp7 = Begin connect~present

d-rma2tnml = Connect account~present

d-tundgq4c = Offer link~present

d-rbyv2265 = Offer sign in~present

d-vzb5ypnh = Complete link~present

d-phhsy6qk = Set sign in~present

d-kk5keo6u = Answer interaction~present

d-mqyhbbzg = Set link photo~present

d-pnmfqivf = Remember avatar~present

d-wcezkqoz = Finish import~present

d-i4tka6nd = Send there~present

d-kwbi63le = Edit there~present

d-6kixfask = Delete there~present

d-eux4s36g = React there~present

d-bdf2gbew = Unreact there~present

d-sxnwvlev = Start thread there~present

d-ekbpcdy4 = Create webhook~present

d-2hpkozcn = Apply channel~present

d-pp3nt6al = Delete channel there~present

d-65mhdjmo = Arrange channels~present

d-436peoz5 = May mirror~present

d-ukq4apoa = May change~present

d-a3tx44xt = Apply role~present

d-movrd2eb = Delete role~present

d-vq7kdv3g = Set member roles~present

d-ltt6f5zq = Kick~present

d-mp6yc4dy = Ban~present

d-fpdjqa7r = Unban~present

d-hueatl55 = Set overwrite~present

d-jvgwhowr = Fetch asset~present

d-jeev5x74 = Apply emoji~present

d-7df37fem = Delete emoji~present

d-t5my6gom = Settle emoji~present

d-c7kj67v3 = Report voice~present
