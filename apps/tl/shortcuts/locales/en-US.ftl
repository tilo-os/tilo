# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-shortcuts = Shortcuts

d-so-tilo-shortcuts-keywords = automation, workflows, automate, scripts, recipes, macros, automator, power automate, kurzbefehle, automatisierung, skripte

d-xe5ybynd = Param
    .doc = One parameter an action takes, as the catalogue names it.
    .noun = { $count ->
        [one] param
       *[other] params
    }
    .untitled = Parameter

d-w7j2mzpu = Name

d-ckvfwyfc = Ty
    .doc = Its type in the catalogue's words: `text`, `int`, `float`, `bool`, `resource`, `object` or `list`.

d-lovculcl = Required

d-dley6sq2 = Description

d-xe5ybynd-filter-required = Required

d-3rke4sat = Member
    .doc = One value an object or a list holds: the name it stands at in the object, or its position in the list.
    .noun = { $count ->
        [one] member
       *[other] members
    }

d-mvlmdz3t = At

d-orfu2qci = Value

d-3rke4sat-filter-value = Value

d-fs2wgpu6 = Bind value
    .doc = A value an argument, a block, an output or a condition is bound to, with what an object or list holds inside it.
    .noun = { $count ->
        [one] bind value
       *[other] bind values
    }
    .untitled = Value

d-a43zwzxm = Kind

d-nu47tafs = Ty
    .doc = Its type in the catalogue's words: `text`, `int`, `bool`, `list`, a record's kind.

d-q5rbcvgr = Name
    .doc = The input or variable it names (with `/` parts into it), an earlier step's id, or a template's text.

d-dzmxkvwn = Pointer
    .doc = The earlier step's output it reads, by its RFC 6901 pointer.

d-bktowd37 = Literal
    .doc = The value typed for a literal, as the person wrote it.

d-iu7blqby = Members
    .doc = An object's members, by the name each stands at, or a list's values in order.

d-fs2wgpu6-filter-kind = Kind

d-mvex4ysy = Condition
    .doc = A branch's condition: a value being set, two values being equal, or conditions joined.
    .noun = { $count ->
        [one] condition
       *[other] conditions
    }

d-5qdigjgy = Kind

d-jm3nn7p2 = Left

d-o4jmv2xp = Right

d-lwqjd6de = Inner
    .doc = The conditions `not`, `all` and `any` hold.

d-mvex4ysy-filter-kind = Kind

d-mvex4ysy-filter-left = Left

d-mvex4ysy-filter-right = Right

d-ntorknma = Choice
    .doc = One value a parameter may be bound to, as the checker accepts it.
    .noun = { $count ->
        [one] choice
       *[other] choices
    }

d-jjnl5c5u = Kind

d-mf4qff6c = Ty

d-kufc2lcn = Name
    .doc = The input or variable it names, an earlier step's id, or a template's text.

d-3uy3tubv = Pointer

d-gouakd6l = Literal

d-bi3zwtrn = Describes

d-ntorknma-filter-kind = Kind

d-rnb2jn2e = Arg
    .doc = One parameter of an action step and where its value comes from.
    .noun = { $count ->
        [one] arg
       *[other] args
    }
    .untitled = Argument

d-jrmg23bl = Name

d-6cqaypb4 = Ty
    .doc = Its type in the catalogue's words.

d-dqswtaus = Pick

d-bqmee32h = Value

d-nq3xfb3y = Choices
    .doc = What the checker accepts for it, read when the draft was last checked.

d-rnb2jn2e-filter-pick = Pick

d-rnb2jn2e-filter-value = Value

d-aw5cxugr = Slot
    .doc = One value a step's block reads (a condition, a repeat's items, a variable's value), kept the way an argument is.
    .noun = { $count ->
        [one] slot
       *[other] slots
    }
    .untitled = Value

d-6l2kcrfo = Name

d-bwqrb4mc = Ty

d-ns6t6yay = Pick

d-a5ejwysp = Value

d-aw5cxugr-filter-pick = Pick

d-aw5cxugr-filter-value = Value

d-zmdhpryg = Problem
    .doc = What the checker found with a draft, at one step (or at the whole of it).
    .noun = { $count ->
        [one] problem
       *[other] problems
    }

d-ddjwwbo3 = Path

d-yyjdqeh7 = Message

d-e7iyfkpj = Grant
    .doc = One thing saving a draft would let it do, in the words a person reads.
    .noun = { $count ->
        [one] grant
       *[other] grants
    }

d-44e6m2wx = Label

d-w6pqc4i2 = Kind

d-bh7m35pe = Step
    .doc = One step of a draft, wherever it sits: its block and its head are stored beside its arguments and the blocks inside it, so two people editing different steps both keep their edit.
    .noun = { $count ->
        [one] step
       *[other] steps
    }
    .untitled = Step

d-h3opbkx5 = Parent
    .doc = The step whose block holds this one; nothing at the top level.

d-wb3ui23u = Branch

d-so6ey5xl = Step id
    .doc = Its own id, which a binding to its output names.

d-vncpsqdq = Head

d-dcuko4cu = App id
    .doc = An action step's action and where the catalogue put it.

d-gc5jsux7 = Action kind

d-srbrc53z = Action name

d-z7jjhgj3 = Action label

d-cvrfcqnx = Resource label

d-m4zdkmap = Selector

d-7sf54hyp = Target
    .doc = The resource it acts on: a stable id, or a kind when the step picks the first one.

d-7jcsp3l3 = Resource binding
    .doc = The binding it picks its resource from, when the resource is known only at the run.

d-dpalqxeo = Params

d-shmtjvi6 = Args

d-lldhjl3c = Slots
    .doc = A branch's condition, a repeat's items, a variable's value and what the agent reads: the bindings each value row holds, by the name the row goes by.

d-rpum3xhw = Condition
    .doc = A branch's condition, as the checker reads one.

d-e6xcw5u3 = Items
    .doc = A repeat's items, as a binding's JSON, and the variable each one takes.

d-zz64m2bs = Variable

d-su76kmlu = Name
    .doc = A variable step's name and value.

d-vsalsfek = Value

d-szzje4pf = Ask name
    .doc = What an ask step asks for.

d-lryaixlb = Ask type

d-nnrjiqq3 = Ask description

d-wkqhz6gh = Prompt
    .doc = What an agent step is told, and what it reads.

d-dlqum4fl = Ai input

d-7kdxq5eg = Ai output

d-xnnuhmxj = Instruction
    .doc = What a task step is to do, what it may use, and the shape of its answer.

d-rq6c5mlz = Allow

d-ptov4iys = Task output

d-yyj53wzo = Read app
    .doc = What a read step reads.

d-qd53kkgv = Read record

d-oihaj3wf = Read joined

d-2f76vazs = Policy
    .doc = What the run does when the step fails.

d-ydlvlxqi = Attempts

d-w3tgejid = Backoff ms

d-b7i6c2gd = Default value

d-bh7m35pe-filter-parent = Parent

d-bh7m35pe-filter-branch = Branch

d-bh7m35pe-filter-head = Head

d-bh7m35pe-filter-selector = Selector

d-bh7m35pe-filter-resource_binding = Resource binding

d-bh7m35pe-filter-condition = Condition

d-bh7m35pe-filter-ask_type = Ask type

d-bh7m35pe-filter-ai_input = Ai input

d-bh7m35pe-filter-read_joined = Read joined

d-bh7m35pe-filter-policy = Policy

d-bh7m35pe-filter-attempts = Attempts

d-bh7m35pe-filter-backoff_ms = Backoff ms

d-bh7m35pe-filter-default_value = Default value

d-bhr6zdw3 = Input
    .doc = One value a shortcut is started with.
    .noun = { $count ->
        [one] input
       *[other] inputs
    }
    .untitled = Input

d-snpnqwju = Name

d-tg4tli3b = Ty

d-m7r4ya7c = Required

d-jgm4hgap = Description

d-zwcz4mo7 = Value
    .doc = What the form that runs the shortcut holds for it; empty until typed.

d-bhr6zdw3-filter-ty = Ty

d-bhr6zdw3-filter-required = Required

d-wdzrcelb = Trigger
    .doc = What starts a saved shortcut on its own.
    .noun = { $count ->
        [one] trigger
       *[other] triggers
    }
    .untitled = Trigger

d-5bbzhstm = Kind

d-nl7rjhv5 = Chord

d-h6z7jmu7 = Hour

d-pwego36a = Minute

d-izj53iqb = Weekdays

d-w5cphrf7 = Catch up

d-jwrxcn3c = Period secs

d-mkcu4vtl = Once ms
    .doc = The one moment it fires at, in milliseconds since the Unix epoch.

d-e3uy6zru = Once text
    .doc = The moment as the person wrote it, which the form edits.

d-4inykrds = App id

d-q7czkxzj = Workspace

d-ehgpmzgw = Folder

d-gcicb4ub = Focus mode

d-xumiu4qy = Focus transition

d-wctuo7yb = Network

d-oim6j5ww = Connected

d-2yej4ltk = Device class

d-bnuqxwzt = Record kind

d-pxnxp2in = Actions

d-uixffjmo = Only fields

d-evffas55 = Actions text
    .doc = The same two as the trigger form edits them, one per comma.

d-q5adiprn = Fields text

d-6zzuwtig = By

d-muql6lws = Event kind

d-gavij27k = From

d-x7wd6xhm = Device

d-7xi6qhpz = Time field

d-dcoqx7cm = Offset secs

d-kr5hkco2 = Filter

d-solyra7s = Source at

d-5pqw4rrp = Every secs

d-wdzrcelb-filter-kind = Kind

d-wdzrcelb-filter-hour = Hour

d-wdzrcelb-filter-minute = Minute

d-wdzrcelb-filter-catch_up = Catch up

d-wdzrcelb-filter-period_secs = Period secs

d-wdzrcelb-filter-once_ms = Once ms

d-wdzrcelb-filter-focus_mode = Focus mode

d-wdzrcelb-filter-focus_transition = Focus transition

d-wdzrcelb-filter-connected = Connected

d-wdzrcelb-filter-by = By

d-wdzrcelb-filter-offset_secs = Offset secs

d-wdzrcelb-filter-every_secs = Every secs

d-xezwr33v = Offer
    .doc = What a person can choose to add in the action picker while it is open: one action an app offers, with what it takes.
    .noun = { $count ->
        [one] offer
       *[other] offers
    }
    .untitled = Action

d-xitbwh3f = App id

d-2oava6yw = App label

d-zuozugvm = Label

d-jufgzesb = Resource

d-bzlonbec = Resource kind

d-jqocub4q = Resource id

d-56tpoja7 = Address

d-gnvisywl = Reads
    .doc = Whether it only reads.

d-fzldwoda = Params

d-xezwr33v-filter-reads = Reads

d-s6khifxk = Program
    .doc = What the agent answered when a person asked it to draft a shortcut.
    .noun = { $count ->
        [one] program
       *[other] programs
    }

d-6ddumpv3 = Inputs

d-zjl7ntoz = Steps

d-tndlzxkx = Triggers

d-nvg74t2f = Output
    .doc = What a finished run reports back; a value of kind `none` reports nothing.

d-hzxfqcak = Step timeout secs
    .doc = How long each step may take, in seconds; zero is the engine's own patience.

d-ywxim34z = Grant
    .doc = The least grant saving it stores, as the engine's rules.

d-s6khifxk-filter-output = Output

d-s6khifxk-filter-step_timeout_secs = Step timeout secs

d-ezkylvvr = Suggested
    .noun = { $count ->
        [one] suggested
       *[other] suggesteds
    }
    .untitled = Suggested shortcut

d-m53kboqe = Name

d-duorb75u = Program
    .doc = The shortcut the agent wrote, read into the same typed program a draft is.

d-pshhv3vt = Problems

d-6e4ysabb = Changes

d-ezkylvvr-filter-program = Program

d-wl2aiz7b = Saved
    .doc = One shortcut saved in the workspace, as the app last read the library.
    .noun = { $count ->
        [one] saved
       *[other] saveds
    }
    .untitled = Shortcut

d-l4rxg53i = Id

d-2hqp5wnu = Name

d-tpetr7if = Program
    .doc = The saved shortcut: its steps, its grant and its triggers.

d-fydnutns = Paused

d-cc5kjsxw = Last call
    .doc = The newest run of it the app started, by its invocation's id, and when it started.

d-44lyca4z = Last started

d-wl2aiz7b-filter-program = Program

d-wl2aiz7b-filter-paused = Paused

d-wl2aiz7b-filter-last_started = Last started

d-flrevobs = Draft
    .doc = A shortcut being written: its inputs, its steps, what it reports and what starts it.
    .noun = { $count ->
        [one] draft
       *[other] drafts
    }
    .untitled = New shortcut

d-qodc2wld = Name

d-imvb73zl = Shortcut id
    .doc = The id it is saved under.

d-jdjr5ygm = Replaces saved

d-qhds5d5u = Program

d-kcuhy6xu = Problems
    .doc = What the checker said when the draft last changed, kept in front of the person while they build it.

d-uzhz35e3 = Grants

d-unbpnmsw = Folded
    .doc = The steps the person folded away, by path, and whether they read the program as text.

d-2j6kqzzx = As text

d-flrevobs-filter-replaces_saved = Replaces saved

d-flrevobs-filter-program = Program

d-flrevobs-filter-as_text = As text

d-w4uftw6l = Workbench
    .doc = The workspace's Shortcuts app: the drafts people are writing, the shortcuts saved from them, and what the agent offered.
    .noun = { $count ->
        [one] workbench
       *[other] workbenches
    }
    .untitled = Shortcuts

d-rwdjyqtz = Drafts

d-daxqqmym = Saved

d-fbadmr47 = Suggested

d-dmzlirxv = Notice
    .doc = What the last save, run or delete came to, and what went wrong, until the next one.

d-ke54krt6 = Failure

d-vjatpjs3 = Form
    .doc = The trigger form's fields, and the trigger it replaces when one is being edited.

d-td3nielz = Editing

d-jpfqt6cc = Sheet
    .doc = Which sheet the window shows (`save` or empty), what the person typed as a description and a refine comment, and what the open picker's filter holds.

d-wb5qv6mt = Description

d-t2nlvsmx = Comment

d-sgiqblcg = Picker query

d-xmyhixxe = New input name

d-jnw4b5oc = New input type

d-olhnz4lr = Ignored
    .doc = The repeated work the person said no to, by its key.

d-45ccshtx = Picker
    .doc = The actions the open picker offers, read when it opened, and the draft and block it adds to.

d-awchuh2j = Picker for

d-rxujaqef = Picker block

d-w4uftw6l-filter-form = Form

d-w4uftw6l-filter-editing = Editing

d-w4uftw6l-filter-new_input_type = New input type

d-w4uftw6l-filter-ignored = Ignored

d-w4uftw6l-filter-picker_for = Picker for

d-skbmu2ve = Place
    .doc = Where a step moves to: the block (a step and its branch, or the top level) and its place there.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-uccg2srx = Parent

d-d5s4qkn6 = Branch

d-lmkmjowh = Index

d-skbmu2ve-filter-parent = Parent

d-skbmu2ve-filter-branch = Branch

d-skbmu2ve-filter-index = Index

d-b3wnciup = Heard
    .noun = { $count ->
        [one] heard
       *[other] heards
    }

d-gjnz55fw = App id

d-ld52mtpg = Action name

d-mflabbsc = Kind

d-obs5it3e = Resource

d-v3scnpbm = Label

d-zbjjfx2e = Repeated
    .noun = { $count ->
        [one] repeated
       *[other] repeateds
    }

d-jhdyurec = Steps

d-qyjikemy = Times

d-zbjjfx2e-filter-times = Times

d-fbvaf4az = Branch
    .doc = Which block of a step the step inside it runs in.

d-yifkvnzx = Top

d-umhp4nos = Then

d-jnywaoht = Otherwise

d-c675ywpt = Each

d-3r6xy7v4 = Head
    .doc = What a step does.

d-xcngzzqa = Invoke

d-jdgtethf = Branch

d-pluru5px = Each

d-dn6pm4id = Set

d-73kmxsxy = Ask

d-2qjrbvyp = Ai

d-4vmyyxsu = Task

d-a7br5bgd = Read

d-5xgz277c = Policy
    .doc = What a run does when a step fails.

d-qz3c6cii = Fail

d-crjmzj5o = Retry

d-2zaakmmg = Fallback

d-i2akc2ue = Carry on

d-5jyasq6t = Selector
    .doc = How a step names the resource its action is called on.

d-dy7w3vzt = Fixed

d-33px26tt = Kind

d-wmxpe3me = Binding

d-c2p7vc4p = Input type
    .doc = The kinds of value a shortcut takes when it is started.

d-uwlyuz5s = Text

d-kqd4w4s5 = Number

d-kcj5b4og = Integer

d-jxyrbubt = Boolean

d-55b3ptft = Date time

d-qvzbxxhl = Resource

d-njht6p7z = Focus mode
    .doc = What a focus-mode trigger watches: Do Not Disturb or the focus schedule.

d-pv3p65a7 = Do not disturb

d-4zsecn6w = Focus

d-re37wwqp = Focus transition
    .doc = Whether it watches the mode switching on or off.

d-yooqmp6p = Enter

d-nfypn5vo = Exit

d-763kcehd = Connected
    .doc = Which network changes a network trigger counts.

d-bhyheqgz = Any

d-73snot2g = Connected

d-p2rykmxo = Disconnected

d-uvn3n33m = By
    .doc = Whose changes a record trigger counts.

d-6l6xdqjm = Anyone

d-v4ceynkf = People

d-wn3cnymp = Not bots

d-f3l7rdas = Trigger kind
    .doc = What starts a saved shortcut on its own.

d-xyztevvm = Keyboard

d-2l5gpq6j = App opened

d-anci7hem = Schedule day

d-fmykbp46 = Schedule every

d-ga2cnbbs = Once

d-cdpshbxz = Workspace joined

d-o6hrxlfi = File changed

d-g77hixtr = Focus mode

d-aoifouh6 = Network changed

d-nftof37h = Device connected

d-5mbh3cvp = Resource changed

d-asyuuam5 = App event

d-mniby2qm = Incoming message

d-zsym3th6 = Nearby device

d-5hk5fwzd = Resource time

d-46dz2hjp = Source changed

d-6pmwvbgt = Bind
    .doc = How a value is held: typed, one of the shortcut's inputs, a variable, an earlier step's output, text with placeholders, or an object or list of values.

d-vr2ukfgy = None

d-llwgqmmi = Literal

d-un7ujz3o = Input

d-2ekvsixw = Variable

d-p7ykyhld = Step

d-cqyhwzni = Template

d-h3n6sqfs = Object

d-t3ku46bc = List

d-6godotzc = Cond
    .doc = What a branch asks.

d-ijmgcg5q = Truthy

d-d7ujtu3e = Equals

d-fq7mwjzd = Negate

d-dk2harbo = All

d-ejxfn3lm = Any

d-gq4nptlt = Pick
    .doc = How a value a person chose is kept: not chosen, one of the values in scope, or a value typed.

d-lh2dybzh = None

d-hrd44x4m = Choice

d-zb77kj66 = Literal

d-ak3axsku = Load value
    .doc = Reads a value the engine wrote into `into`, and what an object or list holds inside it.

d-ak3axsku-value = Value

d-tbowwuxl = Load choice
    .doc = Reads one value the checker offered into `choice`, whose parts are the value's own.

d-tbowwuxl-value = Value

d-74suppjx = Load condition
    .doc = Reads a condition the engine wrote into `c`.

d-74suppjx-value = Value

d-tjsteouu = Load slot
    .doc = A value a step's block reads, as a slot: a literal typed, or the binding that holds it.

d-tjsteouu-s = S

d-tjsteouu-name = Name

d-tjsteouu-value = Value

d-lflueef3 = Load arg

d-lflueef3-name = Name

d-lflueef3-value = Value

d-5p25qal3 = Load saved
    .doc = Reads the program a shortcut is made of into `p`.

d-5p25qal3-value = Value

d-t24ydrbn = Load program

d-t24ydrbn-item = Item

d-bgdrmu2b = Load shortcut
    .doc = A shortcut written as JSON, loaded into `d` as its steps, inputs and triggers.

d-bgdrmu2b-program = Program

d-cykhzuln = Load block

d-cykhzuln-steps = Steps

d-cykhzuln-parent = Parent

d-cykhzuln-branch = Branch

d-bmmwkxsu = Load step

d-bmmwkxsu-item = Item

d-bmmwkxsu-parent = Parent

d-bmmwkxsu-branch = Branch

d-wyvxda4x = Load triggers

d-wyvxda4x-triggers = Triggers

d-g6q4o4bv = New shortcut

d-g6q4o4bv-name = Name

d-ikemghol = Rename

d-ikemghol-d = D

d-ikemghol-name = Name

d-gfdvb6pi = Duplicate draft

d-gfdvb6pi-d = D

d-wlceavbk = Copy block
    .doc = Copies the steps of one block, and every block inside them, into another draft.

d-wlceavbk-to = To

d-wlceavbk-parent = Parent

d-wlceavbk-branch = Branch

d-incblvf5 = Discard draft

d-incblvf5-d = D

d-nrm3x7ih = Import a shortcut

d-nrm3x7ih-shortcut = Shortcut

d-wuzmo3sl = Edit a saved shortcut

d-wuzmo3sl-shortcut = Shortcut

d-znah3mkw = Duplicate saved shortcut

d-znah3mkw-shortcut = Shortcut

d-iewmjpip = Disarm
    .doc = A shortcut loaded as a draft arms nothing: a person saves it first, after the review.

d-bkq25iyp = Open proposed shortcut
    .doc = Open a shortcut the app proposed, named after the work it noticed, as an unsaved draft.

d-bkq25iyp-program = Program

d-74arknyg = Ignore the proposal

d-74arknyg-key = Key

d-5znukzvw = Offer the proposal again

d-5znukzvw-key = Key

d-kk6iwopt = Add step
    .doc = The actions the person is offered, read now, for the open picker.

d-kk6iwopt-d = D

d-kk6iwopt-block = Block

d-wvfrrs5j = Close picker

d-fnpue7n4 = Add offer

d-fnpue7n4-d = D

d-fnpue7n4-o = O

d-zp5be56i = Add block
    .doc = Add a step that is not an app's action: `if`, `for_each`, `set_var`, `ask`, `ai` or `task`.

d-zp5be56i-d = D

d-zp5be56i-head = Head

d-rgpljqfz = Remove

d-rgpljqfz-d = D

d-rgpljqfz-s = S

d-5ggga3k3 = Remove below

d-5ggga3k3-s = S

d-wjzioshk = Move

d-wjzioshk-d = D

d-wjzioshk-s = S

d-wjzioshk-earlier = Earlier

d-tzyii5oc = Move into the step above

d-tzyii5oc-d = D

d-tzyii5oc-s = S

d-brietaym = Move out of its step

d-brietaym-d = D

d-brietaym-s = S

d-mi4wh47g = When this step fails

d-mi4wh47g-d = D

d-mi4wh47g-s = S

d-mi4wh47g-policy = Policy

d-laox62q2 = Adopt choice
    .doc = Copies a value the checker offered into an argument's or a slot's own value.

d-laox62q2-choice = Choice

d-lcxeu4qi = Copy value
    .doc = Copies `from` into `into`, with what an object or a list holds.

d-lcxeu4qi-from = From

d-myqhouwm = Copy condition
    .doc = Copies a condition into `into`, with the conditions it joins.

d-myqhouwm-from = From

d-gowt3a46 = Result

d-gowt3a46-d = D

d-gowt3a46-choice = Choice

d-pmjlvg3z = Clear output

d-pmjlvg3z-d = D

d-v5u74keh = Set slot
    .doc = Where one value a step reads comes from.

d-v5u74keh-d = D

d-v5u74keh-s = S

d-v5u74keh-name = Name

d-v5u74keh-pick = Pick

d-v5u74keh-choice = Choice

d-vxznbnhd = Type slot

d-vxznbnhd-d = D

d-vxznbnhd-s = S

d-vxznbnhd-name = Name

d-m5fvglig = Set argument

d-m5fvglig-d = D

d-m5fvglig-s = S

d-m5fvglig-a = A

d-m5fvglig-pick = Pick

d-m5fvglig-choice = Choice

d-a2hwq4rp = Clear argument

d-a2hwq4rp-d = D

d-a2hwq4rp-s = S

d-a2hwq4rp-a = A

d-scxqojfw = Clear slot

d-scxqojfw-d = D

d-scxqojfw-s = S

d-scxqojfw-name = Name

d-xc3pfce6 = Type argument

d-xc3pfce6-d = D

d-xc3pfce6-s = S

d-xc3pfce6-a = A

d-fwcdlpi3 = Set condition typed

d-fwcdlpi3-d = D

d-fwcdlpi3-s = S

d-fwcdlpi3-text = Text

d-y7lm3fkn = Add input

d-y7lm3fkn-d = D

d-y7lm3fkn-name = Name

d-y7lm3fkn-ty = Ty

d-t4dkj3hv = Remove

d-t4dkj3hv-d = D

d-t4dkj3hv-i = I

d-7bjedqhx = Add trigger

d-7bjedqhx-d = D

d-7bjedqhx-t = T

d-qxfdqt4r = Add trigger
    .doc = Add what the form holds to the open draft, or replace the trigger the form is editing.

d-qxfdqt4r-d = D

d-ndqaad4s = Edit the trigger
    .doc = Load a trigger the draft carries into the form, to change it.

d-ndqaad4s-t = T

d-sysmibg6 = Cancel

d-vrsm6lfa = Remove

d-vrsm6lfa-d = D

d-vrsm6lfa-t = T

d-dm7sdp4m = Apply trigger
    .doc = Copies every field of one trigger onto another.

d-dm7sdp4m-from = From

d-k3howd4y = Review
    .doc = Check the draft, and what each of its steps' arguments may be bound to, so the editor shows a person what saving would accept without the view reading anything itself.

d-2xp5az3t = Match catalogue
    .doc = Keeps every action step's parameters and the type of each argument what the catalogue says they are, reading the catalogue once.

d-7rqdwyok = Apply offer
    .doc = What an offered action says about a step that calls it: its name, its resource, and what every parameter takes.

d-7rqdwyok-offered = Offered

d-evju2tkk = Reload shortcuts

d-u67u56l2 = Save shortcut
    .doc = Save the draft with the least grant saving would store, after the same check the editor shows.

d-u67u56l2-d = D

d-qixxketx = Run
    .doc = Run a saved shortcut with what its inputs hold.

d-qixxketx-s = S

d-6cbwv5q6 = Delete

d-6cbwv5q6-s = S

d-nugwvhum = Pause

d-nugwvhum-s = S

d-o4vsngqk = Resume

d-o4vsngqk-s = S

d-ehedzmnf = Stop the run

d-ehedzmnf-s = S

d-3nvozo2p = Stop every run

d-bmqgcsbw = Agent address
    .doc = The agent's action that drafts, when this workspace is offered it; empty when it is not.

d-vt5ctxzy = Catalogue summary
    .doc = The actions a draft may use, one line each, as the agent is told about them.

d-kwmae5il = Draft it
    .doc = Ask the agent to draft a shortcut doing what the description says.

d-kwmae5il-description = Description

d-aszyebp3 = Refine it
    .doc = Ask the agent what the shortcut should do differently.

d-aszyebp3-s = S

d-aszyebp3-comment = Comment

d-oy6nhmym = Adopt
    .doc = Offer what the agent answered as a draft to open, with what the checker says of it.

d-oy6nhmym-answer = Answer

d-oy6nhmym-changes = Changes

d-uilcp743 = Open as a draft

d-uilcp743-s = S

d-i4yr2ffc = Put it away

d-i4yr2ffc-s = S

d-oqeztzr7 = Toggle fold

d-oqeztzr7-d = D

d-oqeztzr7-path = Path

d-q6ieeg5y = Show sheet

d-q6ieeg5y-sheet = Sheet

d-ezocvbke = Clear new input

d-rhdwpl3q = Toggle day

d-rhdwpl3q-day = Day

d-ezihejx3 = Dismiss

d-hufpezjj = Window
    .w-shortcuts = Shortcuts
    .w-overview = Overview
    .w-new-shortcut = New shortcut
    .w-reload-shortcuts = Reload shortcuts
    .w-drafts = Drafts
    .w-saved = Saved
    .w-that-could-not-be-done = That could not be done
    .w-dismiss = Dismiss
    .w-no-drafts-yet = No drafts yet
    .w-press-new-shortcut-to-start-one = Press New shortcut to start one.
    .w-nothing-open = Nothing open

d-5mjt7ow2 = Overview
    .w-repeated-work = Repeated work
    .w-open-as-a-draft = Open as a draft
    .w-not-this = Not this
    .w-describe-a-shortcut = Describe a shortcut
    .w-tidy-my-inbox-every-morning = Tidy my inbox every morning
    .w-draft-it = Draft it
    .w-the-agent-drafted = The agent drafted “{ $name }”
    .w-put-it-away = Put it away
    .w-no-shortcuts-yet = No shortcuts yet
    .w-describe-one-and-the-agent-drafts = Describe one and the agent drafts it, or press New shortcut and add a step.
    .w-recent-invocations = Recent invocations
    .w-shortcut-run = Shortcut run

d-fghe6gsm = Editor
    .w-name = Name
    .w-show-program-text = Show program text
    .w-add-step = Add step
    .w-review-and-save = Review and save…
    .w-discard = Discard
    .w-program-text = Program text
    .w-switch-back-to-blocks-to-change = Switch back to blocks to change the steps.
    .w-steps = Steps
    .w-add-the-first-step-an-action = Add the first step: an action one of your apps offers, or a block.

d-rw4oxziq = Grant section
    .w-what-saving-would-allow = What saving would allow
    .w-it-uses-no-app-actions-so = It uses no app actions, so it can't read or change anything in your apps.

d-ona7hqdz = Inputs section
    .w-inputs = Inputs
    .w-name = Name
    .w-type = Type
    .w-required = Required
    .w-remove = Remove
    .w-add-input = Add input

d-yogemb7d = Output section
    .w-result = Result
    .w-what-a-finished-run-reports = What a finished run reports
    .w-not-set = Not set

d-j6xkcjgu = Triggers section
    .w-triggers = Triggers
    .w-nothing-starts-it-on-its-own = Nothing starts it on its own yet.
    .w-edit = Edit
    .w-edit-the-trigger = Edit the trigger
    .w-remove = Remove
    .w-what-starts-it = What starts it
    .w-super-shift-n = Super+Shift+N
    .w-so-tilo-notes = so.tilo.notes
    .w-hour = Hour
    .w-minute = Minute
    .w-every-day = Every day
    .w-run-once-when-a-device-was = Run once when a device was off at that time
    .w-seconds-between-runs = Seconds between runs
    .w-run-once-when-a-period-was = Run once when a period was missed
    .w-2026-12-24t18-00 = 2026-12-24T18:00
    .w-any-workspace = Any workspace
    .w-the-folder-s-capability = The folder's capability
    .w-focus = Focus
    .w-switches = Switches
    .w-any-network = Any network
    .w-which-change = Which change
    .w-camera-display-storage = camera, display, storage
    .w-chat-workspace = chat.workspace
    .w-actions-one-per-comma = Actions, one per comma
    .w-whose-changes = Whose changes
    .w-so-tilo-mail = so.tilo.mail
    .w-mail-message = mail.message
    .w-any-app = Any app
    .w-anyone = Anyone
    .w-any-device = Any device
    .w-tasks-issues = tasks.issues
    .w-due-ms = due_ms
    .w-seconds-from-that-time = Seconds from that time
    .w-mail-account = mail.account
    .w-which-one-when-it-has-a = Which one, when it has a name
    .w-look-every-this-many-seconds = Look every this many seconds
    .w-a-filter-as-json-field-equals = A filter, as JSON: field equals value
    .w-apply = Apply
    .w-add-trigger = Add trigger
    .w-cancel = Cancel

d-ok5de5rl = Block
    .w-otherwise = Otherwise

d-ohy3fqxb = Step card
    .w-show-its-steps = Show its steps
    .w-fold = Fold
    .w-move-up = Move up
    .w-move-down = Move down
    .w-into-the-step-above = Into the step above
    .w-move-into-the-step-above = Move into the step above
    .w-out-of-its-step = Out of its step
    .w-move-out-of-its-step = Move out of its step
    .w-add-inside = Add inside
    .w-add-a-step-inside = Add a step inside
    .w-remove = Remove

d-mzopohjn = Step fields
    .w-condition = condition
    .w-condition-2 = Condition
    .w-items = items
    .w-items-2 = Items
    .w-each-item-is-called = Each item is called
    .w-item = item
    .w-variable-name = Variable name
    .w-value = value
    .w-value-2 = Value
    .w-question = Question
    .w-your-answer = Your answer
    .w-answer-type = Answer type
    .w-what-to-do = What to do
    .w-ai-input = ai_input
    .w-what-it-reads = What it reads
    .w-what-the-task-is-to-do = What the task is to do
    .w-app = App
    .w-record = Record
    .w-every-part = Every part
    .w-when-this-step-fails = When this step fails
    .w-tries = Tries
    .w-wait-between-tries-ms = Wait between tries, ms
    .w-default = default
    .w-value-to-go-on-with = Value to go on with

d-wjxq2el3 = Argument row
    .w-not-set = Not set
    .w-type-a-value = Type a value

d-7dsk6mom = Slot row
    .w-not-set = Not set
    .w-type-a-value = Type a value

d-jjzadkgh = Picker
    .w-add-a-step = Add a step
    .w-narrow-by-action-app-or-resource = Narrow by action, app or resource
    .w-close = Close

d-26lxhboy = Saved page
    .w-paused = Paused
    .w-saved-shortcut = Saved shortcut
    .w-run = Run
    .w-resume = Resume
    .w-pause = Pause
    .w-stop-the-run = Stop the run
    .w-edit = Edit
    .w-delete = Delete
    .w-stop-every-run = Stop every run
    .w-last-invocation = Last invocation
    .w-inputs = Inputs
    .w-steps = Steps
    .w-refine-it = Refine it
    .w-say-what-the-last-run-should = Say what the last run should do differently

d-nwwdtzgy = Save sheet
    .w-save-this-shortcut = Save this shortcut?
    .w-saving-lets-this-shortcut-use-the = Saving lets this shortcut use the app actions listed here, and nothing else.
    .w-save-shortcut = Save shortcut
    .w-close = Close
    .w-cancel = Cancel
    .w-it-uses-no-app-actions-so = It uses no app actions, so it can't read or change anything in your apps.
    .w-fix-these-before-saving = Fix these before saving
