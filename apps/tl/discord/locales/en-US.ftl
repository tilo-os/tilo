# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-discord-preview = Discord

d-so-tilo-discord-preview-keywords = discord, chat, server, channels

d-dxitdnte = Discord
    .doc = A connected Discord account: the server it bridges, what the bridge knows of both sides, and who here linked their Discord account.
    .noun = { $count ->
        [one] discord
       *[other] discords
    }

d-sclfytrh = Name
    .doc = What the account is called on Discord.

d-lex22ami = Guild id
    .doc = The server bridged, by its Discord id; empty until one is connected.

d-2mt4scyz = Server
    .doc = The server's name, as Discord shows it.

d-zoqgfxmk = Only channel
    .doc = The one channel bridged, by its Discord id; empty for the whole server.

d-sqlamhjd = Mirror channels
    .doc = Channels made here after the server was adopted are made in Discord too.

d-bxzjsd2v = Map roles
    .doc = The workspace's roles are kept as roles the bridge owns in Discord.

d-jf2mmurb = Ownership
    .doc = Whether the server is still being brought in, or bridged both ways.

d-6hprgb4l = Token item
    .doc = The secret item holding the bot token on this device; the token never enters the resource.

d-k3mrdk3z = Bot user
    .doc = The bot user's Discord id: its own messages and reactions are echoes.

d-pgejogzq = Channels
    .doc = Discord's channels and categories as the bot last observed them.

d-zhmdub7y = Roles
    .doc = Discord's roles as the bot last observed them.

d-aphz34n7 = Emojis
    .doc = The server's custom emoji, as the bot last observed them.

d-ma5o2ina = Map
    .doc = What joins a conversation here and a channel, category, thread, message, webhook, role or emoji there.

d-ec3vplzb = Links
    .doc = The Discord accounts people here linked.

d-pf7f3cf2 = Link offers
    .doc = Discord accounts offered for linking, each to whoever holds its one-time token.

d-kaj3zmhy = Connects
    .doc = People on their way through Discord's consent page, by person.

d-db4wn64y = Sign in page
    .doc = Discord's consent page for signing in through this device, without its state: empty when the device offers none.

d-bkxzddk4 = Sign in url
    .doc = Where a sign-in the person just started sends them, while it waits for what comes back.

d-bdu3hrmn = Avatars
    .doc = A Discord avatar's hash and the address its image is kept at.

d-6g22oebp = Blocked personas
    .doc = Discord accounts whose messages are not mirrored.

d-y6utf74j = Vanished
    .doc = Mapped Discord channels and roles seen once and gone from the latest layout.

d-htrhhc47 = Tilo only
    .doc = Conversations here that stay here: unmatched when the server was adopted, or their Discord side was deleted.

d-b7dtt5ts = Discord only
    .doc = Discord channels and categories that stay there: their side here was deleted.

d-nn6mtwfm = Voice problem
    .doc = Why the bridge cannot carry a voice channel's call now, as Discord refused it; empty while it can.

d-flkdculv = Emoji problem
    .doc = Why the workspace's emoji do not reach the server now; empty while they do.

d-2vnxydnl = Setup open
    .doc = Whether the setup dialog is open.

d-xezmxd63 = Setup wants
    .doc = What the setup waits for the bridge's program to answer: `token`, `find`, `channels` or `sign-in`; empty when nothing waits.

d-idxrloj3 = Setup ask
    .doc = The id of the press the program answers, so an answer to a press the person made before the last one changes nothing.

d-alwcte5y = Setup person
    .doc = The person whose press the program answers: a sign-in is bound to them, not to the program that answered it.

d-6fcfq4y6 = Setup servers
    .doc = The servers the bot is in, as finding them answered.

d-gsce46aw = Setup server
    .doc = The server chosen to bring in, by its Discord id; empty until one is chosen.

d-ewopezjx = Setup server name
    .doc = The name of the server chosen, as Discord shows it.

d-vwt52f57 = Setup channels
    .doc = The channels the server chosen holds, as the program read them, to choose the ones brought in.

d-wuvfepf6 = Setup channel
    .doc = The one channel of the server chosen brought in, by its Discord id; empty for the whole server.

d-xeugfqli = Setup mirror
    .doc = Whether channels made here after the server is brought in are made in Discord too.

d-w2b5pndu = Setup roles
    .doc = Whether the workspace's roles are kept as roles the bridge owns in Discord.

d-pasjy4ku = Setup problem
    .doc = Why the setup's last step did not work, in words the person reads.

d-dxitdnte-filter-mirror_channels = Mirror channels

d-dxitdnte-filter-map_roles = Map roles

d-dxitdnte-filter-ownership = Ownership

d-dxitdnte-filter-blocked_personas = Blocked personas

d-dxitdnte-filter-vanished = Vanished

d-dxitdnte-filter-tilo_only = Tilo only

d-dxitdnte-filter-discord_only = Discord only

d-dxitdnte-filter-setup_open = Setup open

d-dxitdnte-filter-setup_mirror = Setup mirror

d-dxitdnte-filter-setup_roles = Setup roles

d-dszavpot = D channel
    .doc = A Discord channel, thread or category as the bridge last observed it.
    .noun = { $count ->
        [one] d channel
       *[other] d channels
    }

d-7p2rou2g = Id

d-5g2es5cu = Kind

d-i2iwod32 = Name

d-kj47s5k3 = Parent

d-shpntyiy = Position

d-ys7fs4ma = Topic

d-3mwph4ce = Overwrites

d-dszavpot-filter-kind = Kind

d-dszavpot-filter-position = Position

d-ml4oh74n = Overwrite
    .doc = Who a channel's permission overwrite is for, and what it allows and denies.
    .noun = { $count ->
        [one] overwrite
       *[other] overwrites
    }

d-x427uoge = Id

d-76fajf53 = Kind
    .doc = 0 for a role, 1 for a member, as Discord numbers them.

d-2yzg4vng = Allow

d-dbzgqxgr = Deny

d-ml4oh74n-filter-kind = Kind

d-iikcv4dq = D role
    .doc = A Discord role as the bridge last observed it.
    .noun = { $count ->
        [one] d role
       *[other] d roles
    }

d-xom5hhqp = Id

d-aceuzwtj = Name

d-wxuu6jgi = Permissions

d-pddsqdvg = Color

d-xtz54dig = Position

d-zmedxuk3 = Managed

d-iikcv4dq-filter-color = Color

d-iikcv4dq-filter-position = Position

d-iikcv4dq-filter-managed = Managed

d-nikmkvgx = D emoji
    .doc = A Discord custom emoji.
    .noun = { $count ->
        [one] d emoji
       *[other] d emojis
    }

d-oaaqbeyg = Id

d-5vadig7h = Name

d-drktwmcm = Animated

d-nikmkvgx-filter-animated = Animated

d-6bfkcp6q = Mapping
    .doc = What joins a conversation here to its Discord side.
    .noun = { $count ->
        [one] mapping
       *[other] mappings
    }

d-ymr43fiv = Kind

d-qt55olnf = Tilo
    .doc = The conversation's, message's or role's published id here.

d-vzp2s2xi = Discord
    .doc = The Discord id it is carried to.

d-tjidhwme = Origin

d-ozmgf6kq = Persona
    .doc = For a message Discord wrote: its author's account there.

d-4whazn6a = Root
    .doc = For a reply posted in a thread here: its thread's root, as Discord threads are one level.

d-ctwuv3r6 = Picture
    .doc = For an emoji the bridge uploaded: the address of its picture.

d-hj3vtcw2 = Said
    .doc = For a message carried there: what it last said there, so a change here is carried once.

d-j2bra6h6 = Emoji
    .doc = For a message carried there: the emoji the bot put on it there, each once.

d-6bfkcp6q-filter-kind = Kind

d-6bfkcp6q-filter-origin = Origin

d-6bfkcp6q-filter-emoji = Emoji

d-uiign66t = Link
    .doc = A Discord account someone here linked.
    .noun = { $count ->
        [one] link
       *[other] links
    }

d-shknzkt7 = Discord user

d-lvaegbi4 = Person
    .doc = The person here it is linked to.

d-o4oyz2pi = Linked at
    .doc = When it was linked, as ISO text.

d-wivwsyri = User name

d-qxx3lhen = Display name
    .doc = The name the account shows when that is another.

d-i5lrpbng = Avatar
    .doc = The account's Discord avatar hash.

d-e2cawszl = Interaction
    .doc = The slash command's reply that says what came of the link, when it was made from there.

d-bbxol7y7 = Photo
    .doc = The avatar as a small picture, which the person's profile shows until they upload one.

d-fijk4xyy = Photo avatar
    .doc = The avatar hash `photo` is of.

d-lslzqohz = Link offer
    .doc = A Discord account offered for linking, to whoever holds the one-time token it was offered with: only the token's check is kept.
    .noun = { $count ->
        [one] link offer
       *[other] link offers
    }

d-q4uc376y = Check

d-6xuuyuym = Discord user

d-3elvjryr = User name

d-phqjglse = Display name

d-ynydwexj = Avatar

d-am2zudro = Person
    .doc = The person who started it here: only they may take it.

d-umtbptha = Interaction
    .doc = The slash command it answers.

d-lrq3epw3 = Expires
    .doc = When it runs out, as ISO text.

d-x7orbrgf = Connect
    .doc = Someone on their way through Discord's consent page: the round trip's state, hashed, binds what comes back to them.
    .noun = { $count ->
        [one] connect
       *[other] connects
    }

d-3eaw5xx6 = Person

d-ljxvxizk = State sha256

d-jpa253sd = Expires
    .doc = When it runs out, as ISO text.

d-rgyetce5 = Server choice
    .doc = A server the bot is in.
    .noun = { $count ->
        [one] server choice
       *[other] server choices
    }

d-ymsxjog2 = Id

d-tafgiv6q = Name

d-datx3ypp = Send op
    .doc = A message through a channel's webhook, under the person's Discord name.
    .noun = { $count ->
        [one] send op
       *[other] send ops
    }

d-7lcsdnr4 = Op

d-amt4m7mw = Channel

d-3dhjnhjd = Webhook

d-heqsslfh = Thread
    .doc = The thread it goes in, when the conversation is one; none for a channel or a direct conversation.

d-fdgzp6on = Username

d-qje4kbhs = Avatar url

d-nqgp6src = Avatar blob
    .doc = The avatar this message's author wears on Discord, by the hash of its bytes there.

d-d4x237pg = Content

d-ffcddfvf = Attachments

d-zhzpfsvd = Created ms

d-t3tk2ray = Tilo
    .doc = The message here it carries, by its published id.

d-datx3ypp-filter-created_ms = Created ms

d-d3w3t3ph = Out attachment
    .doc = A file a message carries, by the bare blake3 hex of its bytes, as Discord's worker reads it.
    .noun = { $count ->
        [one] out attachment
       *[other] out attachments
    }

d-u7emqycf = Blob

d-zs5m5uxa = Name

d-aeq3264s = Size

d-d3w3t3ph-filter-size = Size

d-l5featsa = Edit op
    .doc = A change to a message the bridge sent.
    .noun = { $count ->
        [one] edit op
       *[other] edit ops
    }

d-2ksixnn3 = Op

d-2sbyakmv = Channel

d-bffu3pev = Webhook

d-bfdmt6yv = Message

d-jb6s4der = Thread

d-raedlkm5 = Content

d-7mrtgcyh = Delete op
    .doc = A message deleted there.
    .noun = { $count ->
        [one] delete op
       *[other] delete ops
    }

d-n5ulezfg = Op

d-n2yclrmw = Channel

d-liw2ay3q = Webhook

d-w6a2lwlt = Message

d-ojxxdcut = Thread

d-lmsfjnvm = React op
    .doc = The bot's reaction added there, or taken back.
    .noun = { $count ->
        [one] react op
       *[other] react ops
    }

d-juyzi4a5 = Op

d-me76z7t7 = Channel

d-orzlvcjf = Message

d-mhrysspc = Emoji

d-4r6e6s2u = Start thread op
    .doc = A thread started on a message.
    .noun = { $count ->
        [one] start thread op
       *[other] start thread ops
    }

d-eqdu4s5n = Op

d-qxyl2yhx = Channel

d-2g4xqyvu = Message

d-2ba4r76l = Name

d-63ujdmb5 = Tilo

d-dc3tq5kz = Create webhook op
    .doc = A channel's webhook made there.
    .noun = { $count ->
        [one] create webhook op
       *[other] create webhook ops
    }

d-p3acbhsm = Op

d-gu2hv6zx = Channel

d-ijnwz6yd = Tilo

d-zr7zqp4s = Channel spec
    .doc = A channel made there, or brought back in line.
    .noun = { $count ->
        [one] channel spec
       *[other] channel specs
    }

d-aid6dwih = Op

d-nyneqhtp = Id
    .doc = The Discord channel it is, when it is known; none for a channel made afresh.

d-344fcxyq = Tilo

d-7qc7rtaa = Kind

d-z5ye67p5 = Name

d-ghxsuztc = Topic

d-yniihhyc = Parent

d-fvy2cnhd = Position

d-zr7zqp4s-filter-kind = Kind

d-zr7zqp4s-filter-position = Position

d-pivxxzdz = Delete channel op
    .doc = A channel deleted there.
    .noun = { $count ->
        [one] delete channel op
       *[other] delete channel ops
    }

d-rxgm3chv = Op

d-cvq5uc5z = Channel

d-j2bgcnxl = Category place
    .doc = Where a category goes.
    .noun = { $count ->
        [one] category place
       *[other] category places
    }

d-n3kocqem = Category

d-tjug6eo2 = Position

d-j2bgcnxl-filter-position = Position

d-vqxe5whi = Channel place
    .doc = Where a channel goes: under `parent` (none for the top level), at `position`.
    .noun = { $count ->
        [one] channel place
       *[other] channel places
    }

d-pposs7uz = Channel

d-w7v7vvgk = Parent

d-rmnsxntg = Position

d-vqxe5whi-filter-position = Position

d-f64u35ho = Arrange op
    .doc = Channels and categories put in their places at once.
    .noun = { $count ->
        [one] arrange op
       *[other] arrange ops
    }

d-4ebcu3vj = Op

d-vamfb4ci = Categories

d-2jxptmbo = Channels

d-5h6a6faz = Role spec
    .doc = A role made there, or brought back in line.
    .noun = { $count ->
        [one] role spec
       *[other] role specs
    }

d-oad2kmtd = Op

d-6khaatsw = Id

d-6fif6ct6 = Tilo

d-x53hs7jf = Name

d-vmrvsfvk = Permissions

d-w2v4ktgb = Color

d-5h6a6faz-filter-color = Color

d-qswlwswr = Delete role op
    .doc = A role deleted there.
    .noun = { $count ->
        [one] delete role op
       *[other] delete role ops
    }

d-wxxprqy5 = Op

d-43gurfsq = Role

d-e64nmhl5 = Member roles op
    .doc = A linked member's roles set there.
    .noun = { $count ->
        [one] member roles op
       *[other] member roles ops
    }

d-kudwdhlw = Op

d-auupacru = User

d-n3pur4nd = Add

d-xgavnnvp = Remove

d-v2rmvdol = Member op
    .doc = A linked member kicked, banned or unbanned there.
    .noun = { $count ->
        [one] member op
       *[other] member ops
    }

d-vpb4svn2 = Op

d-w6fejnzs = User

d-lwbm7m7g = Reason

d-os5gamdn = Overwrite op
    .doc = A channel's permission overwrite set there.
    .noun = { $count ->
        [one] overwrite op
       *[other] overwrite ops
    }

d-hoy7opkc = Op

d-5bwuxkgk = Channel

d-erieg3ch = Target

d-cgrybxyv = Target kind
    .doc = Whose overwrite it is: a role's or a member's, as Discord numbers them.

d-wmuwv3mm = Allow

d-iec2uwfy = Deny

d-os5gamdn-filter-target_kind = Target kind

d-uyjqnbvg = Fetch asset op
    .doc = A file copied from Discord's CDN into the workspace's content store.
    .noun = { $count ->
        [one] fetch asset op
       *[other] fetch asset ops
    }

d-n5gkbaj7 = Op

d-dymgc2vl = Url

d-grilu5ek = Max bytes

d-4xznvc4c = Inline
    .doc = Whether the answer carries the picture itself, as a `data:` address, for a record to keep (an avatar, a link's photo), rather than its bytes reaching the host to store.

d-uyjqnbvg-filter-max_bytes = Max bytes

d-uyjqnbvg-filter-inline = Inline

d-alcqwskz = Answer interaction op
    .doc = A slash command's reply edited to what came of it.
    .noun = { $count ->
        [one] answer interaction op
       *[other] answer interaction ops
    }

d-noppx5yi = Op

d-ecw6352r = Interaction

d-ehjwcbzo = Content

d-orz4jlzu = Emoji spec
    .doc = A workspace emoji uploaded to the server, or renamed there.
    .noun = { $count ->
        [one] emoji spec
       *[other] emoji specs
    }

d-ty6f2nw2 = Op

d-ttzu4a2l = Id

d-bunzoexc = Name

d-7beod5ga = Picture

d-vmrzdpp2 = Tilo

d-z2prgsq7 = Delete emoji op
    .doc = An emoji the bridge made deleted there.
    .noun = { $count ->
        [one] delete emoji op
       *[other] delete emoji ops
    }

d-w7bai4fx = Op

d-h2uwhtax = Emoji

d-3ze5unut = Tilo

d-7yhl3zrz = Perform
    .doc = What the bridge carries out: which account's server, and the call, as JSON.
    .noun = { $count ->
        [one] perform
       *[other] performs
    }

d-x5ajf4ny = Account

d-4flmqbgo = Op

d-5fgcd6rc = Accounts
    .doc = The servers this app bridges: one account per connected Discord server.
    .noun = { $count ->
        [one] account
       *[other] accounts
    }

d-z7et5ezu = Accounts

d-hvcyj2m7 = Voice pair
    .doc = A Discord voice channel and the voice channel here it is carried to: one call, heard on both sides.
    .noun = { $count ->
        [one] voice pair
       *[other] voice pairs
    }

d-5xed4gep = Discord

d-5pvw2ica = Tilo

d-id2ly5b6 = Ownership
    .doc = Whether the server is still being brought in, or bridged both ways.

d-ktssjaxe = Importing

d-e2lygts4 = Adopted

d-ux67zwpw = Mapped kind
    .doc = What a mapping joins.

d-bbba2tmk = Channel

d-auipqkwg = Category

d-2tege6pz = Thread

d-ggczmnw5 = Message

d-ntdowhpo = Webhook

d-7liaqb2v = Role

d-mvo2q5cu = Emoji

d-7cg2sray = Origin side
    .doc = Which side a mapped object was made on.

d-qxzgihfz = Tilo

d-247t6yoc = Discord

d-b4w6fnif = Channel kind
    .doc = What kind of place a Discord channel is.

d-qp7jnxoc = Text

d-bzfgk4zp = Voice

d-ravghdfs = Category

d-5oxolos5 = Thread

d-andmlf26 = Overwrite target
    .doc = Whose permission overwrite an op sets.

d-52qolaok = Role

d-xaec6ivl = Member

d-zl6ascoy = Connect Discord
    .doc = Starts the account: the record the setup writes into.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server

d-tneakrwi = Connect Discord
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server

d-rpivsg2u = Closes it
    .doc = Closes it.

d-pc7v5xfq = Clear problem
    .doc = Forgets what the setup last said went wrong.

d-zkt35fy7 = Add the bot's token
    .doc = Asks the bridge's program to have the bot's token put in the person's vault: the person types it once on the system's own card, and the vault keeps it.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-this-account-already-has-the-bot = This account already has the bot's token

d-movwri4g = Keep token
    .doc = Keeps the vault item the person's token was put in: the program's answer to `ask_token`.
    .w-only-the-bridge-keeps-the-bot = Only the bridge keeps the bot's token where the vault put it
    .w-that-is-no-press-this-setup = That is no press this setup waits on
    .w-a-vault-item-is-named-by = A vault item is named by its address, as vault.item:<id>

d-movwri4g-ask = Ask

d-movwri4g-item = Item

d-qcsgm6nh = Find
    .doc = Finds the servers the bot is in, so one can be chosen: the bridge's program asks Discord with the token the vault keeps.
    .w-only-the-workspace-s-moderators-find = Only the workspace's moderators find the bot's servers
    .w-add-the-bot-s-token-first = Add the bot's token first

d-msbrp3cr = Found servers
    .doc = The servers finding the bot's servers answered: the program's answer to `find_servers`.
    .w-only-the-bridge-answers-what-finding = Only the bridge answers what finding the bot's servers found
    .w-that-is-no-press-this-setup = That is no press this setup waits on

d-msbrp3cr-ask = Ask

d-msbrp3cr-servers = Servers

d-bkp77mea = Choose server
    .doc = The server the person chose to bring in: the program reads its channels so one of them can be brought in alone.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-bkp77mea-s = S

d-whofvy5h = Found channels
    .doc = The server's channels as the program read them: the program's answer to `choose_server`.
    .w-only-the-bridge-answers-what-the = Only the bridge answers what the server's channels are
    .w-that-is-no-press-this-setup = That is no press this setup waits on

d-whofvy5h-ask = Ask

d-whofvy5h-channels = Channels

d-on4tojk2 = Choose channel
    .doc = Chooses the one channel of the server to bring in, or none for the whole server.
    .w-only-the-workspace-s-moderators-choose = Only the workspace's moderators choose the channels
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-on4tojk2-channel = Channel

d-3kbxljv2 = Back to servers
    .doc = Goes back from the channels to the servers, choosing again.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server

d-bh74uc6c = Setup failed
    .doc = Why the setup step `ask` did not work: the program's answer, in words the person reads.
    .w-only-the-bridge-says-why-the = Only the bridge says why the setup's step did not work
    .w-that-is-no-press-this-setup = That is no press this setup waits on

d-bh74uc6c-ask = Ask

d-bh74uc6c-problem = Problem

d-3y5zwmly = Bring it in
    .doc = Binds this account to the server and channels the setup chose.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-choose-the-server-to-bring-in = Choose the server to bring in first

d-k73mzujn = Connect
    .doc = Binds this account to one server.
    .w-only-the-workspace-s-moderators-connect = Only the workspace's moderators connect a Discord server
    .w-this-account-is-already-connected-to = This account is already connected to a server
    .w-a-server-or-channel-id-is = A server or channel id is a number

d-k73mzujn-guild_id = Guild id

d-k73mzujn-name = Name

d-k73mzujn-only_channel = Only channel

d-k73mzujn-mirror_channels = Mirror channels

d-k73mzujn-map_roles = Map roles

d-7vtlvvxl = Observe layout
    .doc = Records Discord's layout as the latest event reported it: channels and roles the layout no longer lists go, the ones it lists are brought up to date, and new ones come in.
    .w-only-the-bridge-keeps-the-server = Only the bridge keeps the server's layout

d-7vtlvvxl-bot_user = Bot user

d-7vtlvvxl-channels = Channels

d-7vtlvvxl-roles = Roles

d-7vtlvvxl-emojis = Emojis

d-sffqebjj = Map
    .doc = Joins a conversation, message or the like here to its Discord side.
    .w-only-the-bridge-joins-the-two = Only the bridge joins the two sides
    .w-a-mapping-joins-two-ids = A mapping joins two ids

d-sffqebjj-kind = Kind

d-sffqebjj-tilo = Tilo

d-sffqebjj-discord = Discord

d-sffqebjj-origin = Origin

d-sffqebjj-persona = Persona

d-sffqebjj-root = Root

d-ifm75o6j = Unmap
    .doc = Forgets a mapping, keeping the two sides apart from now on when asked.
    .w-only-the-bridge-joins-the-two = Only the bridge joins the two sides

d-ifm75o6j-kind = Kind

d-ifm75o6j-tilo = Tilo

d-ifm75o6j-keep_apart = Keep apart

d-r6ym2m7o = Connect
    .doc = Starts signing in with Discord, as the person looking: the bridge's program builds the consent page and the state that binds what comes back to them.
    .w-this-workspace-is-not-bridged-to = This workspace is not bridged to a Discord server
    .w-this-device-does-not-offer-signing = This device does not offer signing in with Discord

d-tyngolsv = Start sign in
    .doc = Where the person goes to sign in with Discord, and the digest of the state that binds what comes back to them: the program's answer to `connect_account`.
    .w-only-the-bridge-starts-a-sign = Only the bridge starts a sign-in with Discord
    .w-that-is-no-press-this-setup = That is no press this setup waits on
    .w-a-consent-page-is-an-https = A consent page is an https address
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-tyngolsv-ask = Ask

d-tyngolsv-url = Url

d-tyngolsv-sha256 = Sha256

d-zxwz5hgy = Offer link
    .doc = Offers a Discord account for linking, from the server: `/link` answered with a one-time token, whose check this is.
    .w-only-the-bridge-offers-a-link = Only the bridge offers a link
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits

d-zxwz5hgy-check = Check

d-zxwz5hgy-discord_user = Discord user

d-zxwz5hgy-user_name = User name

d-zxwz5hgy-display_name = Display name

d-zxwz5hgy-avatar = Avatar

d-zxwz5hgy-interaction = Interaction

d-ozurd2aj = Offer sign in
    .doc = Offers the account that signed in on Discord's consent page, to the person who started the round trip `state_sha256` names.
    .w-only-the-bridge-offers-a-link = Only the bridge offers a link
    .w-a-digest-is-64-hex-digits = A digest is 64 hex digits
    .w-that-sign-in-with-discord-was = That sign-in with Discord was not started here, or took too long

d-ozurd2aj-check = Check

d-ozurd2aj-discord_user = Discord user

d-ozurd2aj-user_name = User name

d-ozurd2aj-state_sha256 = State sha256

d-ozurd2aj-display_name = Display name

d-ozurd2aj-avatar = Avatar

d-emnquv7s = Keep offer
    .doc = Keeps an offer for ten minutes, dropping those run out.
    .w-only-the-bridge-keeps-an-offer = Only the bridge keeps an offer

d-emnquv7s-offered = Offered

d-m7tzo2pv = Complete link
    .doc = Links the Discord account a one-time token offers to the person looking.
    .w-this-link-was-not-started-here = This link was not started here
    .w-this-link-was-already-used-or = This link was already used or has run out. Start again from Discord with /link
    .w-this-link-ran-out-after-ten = This link ran out after ten minutes. Start again with /link
    .w-this-link-was-started-by-another = This link was started by another account. Sign in as that account, or start again

d-m7tzo2pv-proof = Proof

d-t6hjvvtg = Set sign in
    .doc = Offers signing in with Discord: this device's consent page, without its state; empty for none.
    .w-only-the-workspace-s-moderators-set = Only the workspace's moderators set up signing in with Discord
    .w-a-consent-page-is-an-http = A consent page is an http(s) address with its query

d-t6hjvvtg-page = Page

d-tvemn4rz = Answer interaction
    .doc = Edits a slash command's reply to what came of it.
    .w-only-the-bridge-answers-a-slash = Only the bridge answers a slash command
    .w-a-reply-holds-1-to-2000 = A reply holds 1 to 2000 characters

d-tvemn4rz-op = Op

d-evtumcea = Rename link
    .doc = Takes a linked account's newer names and avatar.
    .w-only-the-bridge-takes-new-names = Only the bridge takes new names for a link

d-evtumcea-discord_user = Discord user

d-evtumcea-user_name = User name

d-evtumcea-display_name = Display name

d-evtumcea-avatar = Avatar

d-k27e5op2 = Set link photo
    .doc = Keeps a linked account's avatar as the person's picture, until they upload their own.
    .w-only-the-bridge-keeps-a-link = Only the bridge keeps a link's picture
    .w-a-picture-is-a-base64-data = A picture is a base64 data: URL of a PNG, JPEG or WebP

d-k27e5op2-discord_user = Discord user

d-k27e5op2-avatar = Avatar

d-k27e5op2-photo = Photo

d-sj2gml32 = Unlink
    .doc = Unlinks a Discord account: its person's, or the bridge's.
    .w-only-the-workspace-s-moderators-or = Only the workspace's moderators, or the person it is with, unlink a Discord account

d-sj2gml32-discord_user = Discord user

d-fmpqddw3 = Unlink
    .doc = Unlinks the person's own Discord account.

d-oqytjrjj = Block persona
    .doc = Stops mirroring a Discord account: its messages are no longer carried, and the ones already here are removed.
    .w-only-the-workspace-s-moderators-block = Only the workspace's moderators block a Discord account

d-oqytjrjj-discord_user = Discord user

d-fq2gat4l = Unblock persona
    .doc = Mirrors a Discord account again.
    .w-only-the-workspace-s-moderators-block = Only the workspace's moderators block a Discord account

d-fq2gat4l-discord_user = Discord user

d-zqdfbauh = Remember avatar
    .doc = Remember where an avatar's image is stored.
    .w-only-the-bridge-keeps-an-avatar = Only the bridge keeps an avatar
    .w-an-avatar-hash-and-a-content = An avatar hash and a content address

d-zqdfbauh-hash = Hash

d-zqdfbauh-blob = Blob

d-f23gpsje = Finish import
    .doc = Finishes adopting the server: everything in it is here now.
    .w-only-the-bridge-finishes-the-import = Only the bridge finishes the import
    .w-the-server-was-already-adopted = The server was already adopted

d-f23gpsje-tilo_only = Tilo only

d-k3qmcewh = Carry native discord
    .doc = The Discord server the app of old kept, carried once into this app's record: the joinings of both sides (channels, threads, messages, webhooks, roles and emoji), the accounts people linked, the blocked accounts, and the one channel the bridge is confined to.

d-zry53cq3 = Send there
    .doc = Sends a message through a channel's webhook, as the bridge plans it.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters
    .w-discord-takes-at-most-ten-files = Discord takes at most ten files of 10 MiB from a webhook

d-zry53cq3-op = Op

d-j7j3sioa = Edit there
    .doc = Edits a message the bridge sent.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks
    .w-a-discord-message-is-1-to = A Discord message is 1 to 2000 characters

d-j7j3sioa-op = Op

d-7dzstxbt = Delete there
    .doc = Deletes a message there.
    .w-the-bridge-carries-messages-to-discord = The bridge carries messages to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-that-is-not-one-of-the = That is not one of the bridge's webhooks

d-7dzstxbt-op = Op

d-2yapf5ci = React there
    .doc = Adds the bot's reaction.
    .w-the-bridge-carries-reactions-to-discord = The bridge carries reactions to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-2yapf5ci-op = Op

d-ckxzivok = Unreact there
    .doc = Takes the bot's reaction back.
    .w-the-bridge-carries-reactions-to-discord = The bridge carries reactions to Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-ckxzivok-op = Op

d-7m3iknds = Start thread there
    .doc = Starts a thread on a message.
    .w-the-bridge-starts-threads-on-discord = The bridge starts threads on Discord
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-7m3iknds-op = Op

d-higzpy27 = Create webhook
    .doc = Creates a channel's webhook.
    .w-the-bridge-makes-the-server-s = The bridge makes the server's webhooks
    .w-there-is-no-such-channel-in = There is no such channel in the server

d-higzpy27-op = Op

d-wihi5xtl = Apply channel
    .doc = Creates a channel there, or brings it back in line.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-a-discord-channel-name-is-1 = A Discord channel name is 1 to 100 characters

d-wihi5xtl-op = Op

d-eginj4va = Delete channel there
    .doc = Deletes a channel there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-eginj4va-op = Op

d-2xvzq6ke = Arrange channels
    .doc = Puts channels and categories in their places there, so the server lists them in this order.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's channels
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-moves-only-the-channels = The bridge moves only the channels it bridges to another category

d-2xvzq6ke-op = Op

d-7ilqj4nx = May mirror
    .doc = Says whether the bridge may change the server's channels: mirroring is on and the server is adopted.
    .w-this-bridge-does-not-make-channels = This bridge does not make channels in Discord
    .w-the-server-is-adopted-first = The server is adopted first

d-5n6nirxx = May change
    .doc = Says whether the bridge made the Discord channel or category `id` for one here.
    .w-the-bridge-changes-only-channels-it = The bridge changes only channels it made, never the server's own

d-5n6nirxx-id = Id

d-hfrhxk34 = Apply role
    .doc = Creates a role there, or brings it back in line.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-the-bridge-changes-only-the-roles = The bridge changes only the roles it made

d-hfrhxk34-op = Op

d-o6etfdzd = Delete role
    .doc = Deletes a role there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-the-bridge-deletes-only-roles-it = The bridge deletes only roles it manages

d-o6etfdzd-op = Op

d-ouxno4io = Set member roles
    .doc = Sets a linked member's roles there.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's roles
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here
    .w-the-bridge-assigns-only-roles-it = The bridge assigns only roles it manages

d-ouxno4io-op = Op

d-gvqt3lkz = Kick
    .doc = Kicks a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-gvqt3lkz-op = Op

d-mygpdagq = Ban
    .doc = Bans a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-mygpdagq-op = Op

d-xf3g66fn = Unban
    .doc = Unbans a linked member there.
    .w-only-the-workspace-s-moderators-manage = Only the workspace's moderators manage Discord members
    .w-that-discord-account-is-not-linked = That Discord account is not linked to anyone here

d-xf3g66fn-op = Op

d-a66erer2 = Set overwrite
    .doc = Sets a channel's permission overwrite, for a role the bridge made.
    .w-only-the-workspace-s-moderators-change = Only the workspace's moderators change the server's permissions
    .w-this-bridge-leaves-the-server-s = This bridge leaves the server's roles alone
    .w-there-is-no-such-channel-in = There is no such channel in the server
    .w-the-bridge-sets-overwrites-only-for = The bridge sets overwrites only for the roles it made

d-a66erer2-op = Op

d-yxtsvom4 = Fetch asset
    .doc = Copies a file from Discord's CDN into the workspace's content store.
    .w-the-bridge-copies-discord-s-files = The bridge copies Discord's files
    .w-chat-holds-files-of-at-most = Chat holds files of at most 32 MiB

d-yxtsvom4-op = Op

d-2a3sj64d = Apply emoji
    .doc = Uploads a workspace emoji to the server, or renames one the bridge uploaded.
    .w-a-discord-emoji-s-name-is = A Discord emoji's name is 2 to 32 letters, digits and underscores
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded
    .w-an-emoji-op-names-its-picture = An emoji op names its picture's content address

d-2a3sj64d-op = Op

d-e5746r6f = Delete emoji
    .doc = Deletes an emoji the bridge uploaded.
    .w-the-bridge-changes-only-emoji-it = The bridge changes only emoji it uploaded

d-e5746r6f-op = Op

d-kdvqv4wx = Settle emoji
    .doc = Records what became of an emoji upload, rename or delete.
    .w-only-the-bridge-settles-an-emoji = Only the bridge settles an emoji
    .w-an-emoji-problem-is-1-to = An emoji problem is 1 to 300 characters

d-kdvqv4wx-tilo = Tilo

d-kdvqv4wx-emoji_id = Emoji id

d-kdvqv4wx-problem = Problem

d-kdvqv4wx-picture = Picture

d-kdvqv4wx-was = Was

d-kdvqv4wx-deleted = Deleted

d-harljhzx = Report voice
    .doc = Says why the bridge cannot carry voice now, or that it can again.
    .w-only-the-bridge-reports-what-discord = Only the bridge reports what Discord said of a call
    .w-a-voice-problem-is-1-to = A voice problem is 1 to 300 characters

d-harljhzx-problem = Problem

d-mslrzlnl = Pump
    .doc = A value nothing reads, for the trigger's frame that does nothing itself: the origin polls Discord's events and the conversations here in its own loop.

d-rohfey6t = Accounts view
    .w-bring-a-discord-server-into-this = Bring a Discord server into this workspace
    .w-its-channels-become-conversations-here-and = Its channels become conversations here, and what people say on either side shows on both.
    .w-connect-discord = Connect Discord

d-igfvovlh = Begin setup~present

d-3bzkicj7 = Account
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
    .w-continue-signing-in-with-discord = Continue signing in with Discord
    .w-link-your-discord-account-from-the = Link your Discord account from the server with /link
    .w-linked = Linked
    .w-the-server = The server

d-ogaewyqt = Setup dialog
    .w-connect-a-discord-server = Connect a Discord server
    .w-cancel = Cancel
    .w-find-the-bot-s-servers = Find the bot's servers
    .w-choose-the-server-to-bring-in = Choose the server to bring in.

d-nwypke43 = Token step
    .w-the-bot-s-token-is-pasted = The bot's token is pasted into your vault, never into this workspace's records: you type it once on the card that asks you for it, and this account keeps only where the vault put it.
    .w-add-the-bot-s-token = Add the bot's token
    .w-the-bot-s-token-is-kept = The bot's token is kept
    .w-in-your-vault-on-this-device = In your vault, on this device's side of the workspace.

d-5lmjieja = Channel step
    .w-this-server-holds-no-channel-to = This server holds no channel to bring in.
    .w-choose-the-one-channel-to-bring = Choose the one channel to bring in, or none for the whole server.
    .w-call = Call
    .w-channels-made-here-are-made-in = Channels made here are made in Discord too
    .w-the-workspace-s-roles-are-kept = The workspace's roles are kept as this server's
    .w-back = Back
    .w-bring-it-in = Bring it in

d-eqrl7f23 = Open setup~present

d-evqpxh3x = Ask token~present

d-rran2vmo = Keep token~present

d-zv3haunw = Find servers~present

d-b2pe4evc = Found servers~present

d-qb3uanfd = Choose server~present

d-4whvn4se = Found channels~present

d-ypq6z5ao = Choose channel~present

d-4peaolpo = Back to servers~present

d-rhzifqce = Setup failed~present

d-zdsjcfdz = Bridge server~present

d-zuoxo3lk = Connect~present

d-nes6e3bj = Observe layout~present

d-quzk7plq = Map~present

d-dh6uikuf = Unmap~present

d-cd5gzjfe = Connect account~present

d-hy7asmwn = Start sign in~present

d-b32fs3at = Offer link~present

d-rb65cd7t = Offer sign in~present

d-y727oaux = Keep offer~present

d-hrryc5cp = Complete link~present

d-mkojtijn = Set sign in~present

d-djx423kw = Answer interaction~present

d-oowjdkgs = Rename link~present

d-spaj33la = Set link photo~present

d-b5xgcayi = Unlink~present

d-u5rlyq7p = Block persona~present

d-jdamvkmo = Unblock persona~present

d-ufrv6cc2 = Remember avatar~present

d-hhs57om3 = Finish import~present

d-ciurnhe7 = Send there~present

d-bnegqdjf = Edit there~present

d-zzvmn4xi = Delete there~present

d-4iwehiki = React there~present

d-xp6r6bin = Unreact there~present

d-7bn6nqpj = Start thread there~present

d-vzsrzbem = Create webhook~present

d-jjlmbq4p = Apply channel~present

d-ycxenvv2 = Delete channel there~present

d-apkhuebj = Arrange channels~present

d-euc4siys = May mirror~present

d-5u2awqoo = May change~present

d-wciazrc3 = Apply role~present

d-k4id7lbs = Delete role~present

d-rcplwxlk = Set member roles~present

d-qjidl6tn = Kick~present

d-55hp2hm5 = Ban~present

d-m6i6h62c = Unban~present

d-xg2il2dx = Set overwrite~present

d-pvcoaawp = Fetch asset~present

d-zdv75hid = Apply emoji~present

d-xnzu556b = Delete emoji~present

d-gh6ivpqd = Settle emoji~present

d-q52y6pdf = Report voice~present
