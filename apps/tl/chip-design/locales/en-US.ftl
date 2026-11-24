# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-chip-design = Chip Design

d-so-tilo-chip-design-keywords = chip, chip design, chip-design, operator, operator graph, hardware, verification, report, evidence, rtl, netlist, silicon, chip-entwurf, halbleiter, nachweis

d-xa35avfl = Port
    .doc = One typed port of an operator.
    .noun = { $count ->
        [one] port
       *[other] ports
    }

d-esvx2n72 = Key
    .doc = Its identity on its operator: `value`, `weights`, `result`, `left` or `right`.

d-vjm4mdax = Data type
    .doc = The tensor type it moves.

d-ixbog3pv = Operator
    .doc = One operator of the structural design.
    .noun = { $count ->
        [one] operator
       *[other] operators
    }

d-wxkbwcn5 = Node
    .doc = Its stable identity.

d-e6aj4t7l = Kind

d-nbs52ikt = Shape
    .doc = The F32 tensor shape; its last axis is the normalization width.

d-w6rehfmk = Normalization width

d-gonrdrls = Output width
    .doc = The structural matrix output width, when the operator has a matrix.

d-u3ws6vjg = Has output width

d-nreyvv4r = Shape text
    .doc = What a person typed for the shape, applied by `apply_shape`.

d-sjbeclxu = Width text
    .doc = What a person typed for the matrix output width, applied by `apply_width`.

d-s4l6n47t = X

d-pgjl7vwe = Y

d-neb6c6b5 = Inputs

d-eb6j7t6x = Outputs

d-o4fzd6at = Connection
    .doc = One connection between two operator ports.
    .noun = { $count ->
        [one] connection
       *[other] connections
    }

d-sqqjxtyw = From node

d-hk3nk2x5 = From port

d-xgc3rbc2 = To node

d-hxqnvwsp = To port

d-3tc244az = Digest
    .doc = The content identity of the connection.

d-o2gagtnb = Active
    .doc = Whether the stored design still connects it.

d-qkwwqrxp = Reason
    .doc = Why it does not, when it does not.

d-o4fzd6at-filter-active = Active

d-hayxmj5x = Assumptions
    .doc = The storage assumptions a report states.
    .noun = { $count ->
        [one] assumption
       *[other] assumptions
    }

d-run6wd4l = Logical parameters

d-smx2rwim = Weight bits

d-kclrpitf = Kv bits

d-6r63anpq = Context tokens

d-hayxmj5x-filter-logical_parameters = Logical parameters

d-hayxmj5x-filter-weight_bits = Weight bits

d-hayxmj5x-filter-kv_bits = Kv bits

d-hayxmj5x-filter-context_tokens = Context tokens

d-mwnbuncs = Report
    .doc = One analytical storage report, as imported and as a person reads it.
    .noun = { $count ->
        [one] report
       *[other] reports
    }

d-twijrbwn = Digest
    .doc = The content identity of the imported bytes.

d-7rep5j6b = Source
    .doc = The exact imported report, as the producer wrote it.

d-hd2g3vdr = Model
    .doc = The model repository the report is about.

d-fxx5zcjz = Schema version

d-svry2b7t = Revision

d-xyjuenxt = Config sha256

d-vyv4me7w = Evidence

d-b3g6lqvm = Batch size

d-5qj7upc2 = Assumptions

d-42kdgwvs = Packed weight bytes

d-7jvkir3f = Cache

d-qw2v536a = Cache reason

d-ogp5wc76 = Attention layers

d-he3itm4b = Kv elements

d-biifjz5l = Kv bytes

d-n3tcruqh = Cache excludes

d-mrp773gq = Not estimated

d-oypgaywq = Limitations

d-gdfhr7j4 = Check
    .doc = One reported check.
    .noun = { $count ->
        [one] check
       *[other] checks
    }

d-i2clfcou = Name

d-arunp5tg = Result

d-gdfhr7j4-filter-result = Result

d-wv7oxuhd = Tool
    .doc = One tool identity a receipt reports.
    .noun = { $count ->
        [one] tool
       *[other] tools
    }

d-6l36tmym = Name

d-ptuif53v = Version

d-6ffjwxk2 = Executable sha256

d-kkcqj2np = Artifact
    .doc = One artifact identity a receipt reports.
    .noun = { $count ->
        [one] artifact
       *[other] artifacts
    }

d-4d7avciq = Name

d-nwa3vepp = Sha256

d-umlywiy3 = Digest
    .doc = One named digest a receipt reports.
    .noun = { $count ->
        [one] digest
       *[other] digests
    }

d-r4lv3eu7 = Name

d-pn5tgrsn = Sha256

d-w7znau2a = Cell count
    .doc = One generic cell count a superblock receipt reports.
    .noun = { $count ->
        [one] cell count
       *[other] cell counts
    }

d-r4l4iqx2 = Name

d-vxrpbdeb = Count

d-w7znau2a-filter-count = Count

d-hmpy65v2 = Observation
    .doc = One numerical observation of a CPU operator receipt.
    .noun = { $count ->
        [one] observation
       *[other] observations
    }

d-l3my4c7u = Label
    .doc = What the observation compared, in words.

d-boatafil = Kind

d-jbgmt4ie = Comparison
    .doc = What the comparison asserts.

d-2m7xr7oi = Compared words

d-765n5dvn = Differing words

d-r2sgywxz = Output sha256

d-2mvyzxqy = Outcome

d-jqiqarxm = Refused inputs

d-7g5ljuf2 = Cycles

d-tkyyux4t = Accepted inputs

d-byvm4brz = Accepted outputs

d-ysqpkcxy = Logic cells

d-fmeekkru = Logical memory bits

d-fswivmpb = Library

d-e5eeqpa7 = Clock constraint ns

d-orrpic3k = Setup slack ns

d-pbdqj47x = Hold slack ns

d-j7qjxpgt = Routed signal nets

d-js7iwiwx = Router violations

d-bsogreds = Library area um2

d-62qim76q = Profile
    .doc = The RMS profile this comparison ran over.

d-vdf52bkl = Fp32 comparisons

d-qvzkneov = Total inputs
    .doc = SiLU's finite inputs and its exponential differences.

d-sofq7z7n = Exp differences

d-rgsgq4i3 = Max exp ulp

d-fwsjrjk3 = Heads
    .doc = Gated-delta shape and the words it compared.

d-shn33gs4 = Tokens

d-fn6veddp = Width

d-d3y6qjsu = Attention words

d-rfak7s5n = State words

d-3mbqqhmz = Name
    .doc = Softmax's masked inputs and its bounded differences.

d-hif4kqoj = Masked

d-3ijpmjgv = Max ulp

d-hmpy65v2-filter-kind = Kind

d-hmpy65v2-filter-comparison = Comparison

d-hmpy65v2-filter-compared_words = Compared words

d-hmpy65v2-filter-differing_words = Differing words

d-hmpy65v2-filter-outcome = Outcome

d-hmpy65v2-filter-refused_inputs = Refused inputs

d-hmpy65v2-filter-cycles = Cycles

d-hmpy65v2-filter-accepted_inputs = Accepted inputs

d-hmpy65v2-filter-accepted_outputs = Accepted outputs

d-hmpy65v2-filter-logic_cells = Logic cells

d-hmpy65v2-filter-logical_memory_bits = Logical memory bits

d-hmpy65v2-filter-routed_signal_nets = Routed signal nets

d-hmpy65v2-filter-router_violations = Router violations

d-hmpy65v2-filter-fp32_comparisons = Fp32 comparisons

d-hmpy65v2-filter-total_inputs = Total inputs

d-hmpy65v2-filter-exp_differences = Exp differences

d-hmpy65v2-filter-max_exp_ulp = Max exp ulp

d-hmpy65v2-filter-heads = Heads

d-hmpy65v2-filter-tokens = Tokens

d-hmpy65v2-filter-width = Width

d-hmpy65v2-filter-attention_words = Attention words

d-hmpy65v2-filter-state_words = State words

d-hmpy65v2-filter-masked = Masked

d-hmpy65v2-filter-max_ulp = Max ulp

d-yxlbi32g = Memory
    .doc = One logical memory a reported synthesis observed.
    .noun = { $count ->
        [one] memory
       *[other] memories
    }

d-imhcxbpl = Name

d-eifw7p4n = Width bits

d-474at3uz = Depth words

d-o76j7m2f = Bits

d-2aqawzb3 = Read ports

d-he5exqe2 = Clocked read ports

d-6nk642cp = Write ports

d-hovn72p4 = Fully initialized

d-yxlbi32g-filter-width_bits = Width bits

d-yxlbi32g-filter-depth_words = Depth words

d-yxlbi32g-filter-bits = Bits

d-yxlbi32g-filter-read_ports = Read ports

d-yxlbi32g-filter-clocked_read_ports = Clocked read ports

d-yxlbi32g-filter-write_ports = Write ports

d-yxlbi32g-filter-fully_initialized = Fully initialized

d-bjljl2tc = Variant
    .doc = One variant of a reported architecture comparison.
    .noun = { $count ->
        [one] variant
       *[other] variants
    }

d-zl5mvubo = Identity

d-4y2sweqe = Numerical
    .doc = The numerical comparison, when the stage reports one.

d-qe2ijn52 = Cold program cycles

d-lvk3uaeb = Vector load cycles

d-pm72baqp = Warm cycles

d-bctwv4iq = Accepted weight bytes

d-sbhcuyji = Accepted descriptor bytes

d-lrpqhjtv = Cache reads

d-chkz2y4j = Quantizer packets

d-f7om2r4s = Comparison words

d-5p4i7krn = Output sha256

d-vte7iosv = Synthesis
    .doc = The generic synthesis this variant observed, when the stage reports one.

d-z3aiymm6 = Generic logic cells

d-wsvyc46d = Activation cache bits

d-ouotgooh = Programmed weight bits

d-htrm4etw = Fixed initialized bits

d-s3urh6zt = Memories

d-bjljl2tc-filter-numerical = Numerical

d-bjljl2tc-filter-cold_program_cycles = Cold program cycles

d-bjljl2tc-filter-vector_load_cycles = Vector load cycles

d-bjljl2tc-filter-warm_cycles = Warm cycles

d-bjljl2tc-filter-accepted_weight_bytes = Accepted weight bytes

d-bjljl2tc-filter-accepted_descriptor_bytes = Accepted descriptor bytes

d-bjljl2tc-filter-cache_reads = Cache reads

d-bjljl2tc-filter-quantizer_packets = Quantizer packets

d-bjljl2tc-filter-comparison_words = Comparison words

d-bjljl2tc-filter-synthesis = Synthesis

d-bjljl2tc-filter-generic_logic_cells = Generic logic cells

d-bjljl2tc-filter-activation_cache_bits = Activation cache bits

d-bjljl2tc-filter-programmed_weight_bits = Programmed weight bits

d-bjljl2tc-filter-fixed_initialized_bits = Fixed initialized bits

d-urioql56 = Stage link
    .doc = One stage link of a reported architecture comparison.
    .noun = { $count ->
        [one] stage link
       *[other] stage links
    }

d-2bpp44k7 = Name

d-cphzthhu = Receipt sha256

d-vnv3rjs3 = Producer sha256

d-u5cz3l65 = Input identity sha256

d-ceqqtftx = Stage
    .doc = One stage of a reported evidence flow.
    .noun = { $count ->
        [one] stage
       *[other] stages
    }

d-4qfzbkq5 = Key
    .doc = The producer's identity for the stage.

d-htbbatar = Title

d-wyge75zm = Status

d-uglh3san = Original receipt artifact

d-46xvj5o3 = Producer sha256

d-775i2e3d = Input identity sha256

d-vxt23uic = Limitations

d-zistzyfl = Observed

d-4nxr5eoq = Observation

d-vqhz7zzv = Checks

d-znevuybg = Placed
    .doc = The reported macro placement, when the stage reports one.

d-6mwfx57p = Database artifact

d-t4u6picr = Observation artifact

d-jquleaxm = Dbu per micron

d-abpu3jku = Die left

d-zgxyfzp2 = Die bottom

d-cfo4rbhf = Die right

d-ozebffaf = Die top

d-lk5ymnxh = Total instances

d-linro3fe = Omitted instances

d-l7433uoi = Macros

d-twlssaox = Macro
    .doc = One macro a reported placement holds.
    .noun = { $count ->
        [one] macro
       *[other] macros
    }

d-l2pfdak5 = Slot
    .doc = The reported macro identity.

d-shgem24v = Name

d-oo4l5j2f = Master

d-tpbo6p6i = Orientation

d-mvnygfkc = Placement

d-sb4fvyic = Left

d-uwbrid6d = Bottom

d-bdrtkvnd = Right

d-2f6kftyj = Top

d-twlssaox-filter-orientation = Orientation

d-twlssaox-filter-left = Left

d-twlssaox-filter-bottom = Bottom

d-twlssaox-filter-right = Right

d-twlssaox-filter-top = Top

d-654bphdp = Dependency
    .doc = One reported evidence dependency.
    .noun = { $count ->
        [one] dependency
       *[other] dependencies
    }

d-lifw33iv = From

d-lau4lbgx = To

d-vuoz6p3k = Kind

d-flo3szj2 = Artifact

d-654bphdp-filter-kind = Kind

d-v72w2lcr = Subflow
    .doc = One reported subflow an evidence flow references.
    .noun = { $count ->
        [one] subflow
       *[other] subflows
    }

d-utp25rwu = Key
    .doc = The producer's identity for the referenced subflow.

d-2hn4pjda = Title

d-qhfawhwq = Summary artifact

d-s6kcjx6i = Receipt
    .doc = One imported hardware verification receipt, as a person reads it.
    .noun = { $count ->
        [one] receipt
       *[other] receipts
    }

d-mhsaondd = Digest
    .doc = The identity of the sanitized receipt this project keeps.

d-nw7f6brj = Source
    .doc = The sanitized receipt, as this project keeps it.

d-x37f53ms = Evidence

d-nj3b4qpo = Status

d-jhyibxbz = Imported bytes sha256

d-3zyb62tt = Failure

d-zon6lior = Failure stage

d-bp2hd5ou = Input validation

d-sp7jdjll = Has input validation

d-gcb6foq7 = Lanes

d-ufj5tgp7 = Has lanes

d-ntsw3ejx = Group size

d-t5yt4cgs = Has group size

d-ghx5dlez = Declared toolchain image

d-sofs7yem = Sample manifest sha256

d-tu4t57su = Verifier sha256

d-qvghxafg = Artifact validator sha256

d-snhxqhnq = Mapper sha256

d-7ifmtq3n = Library revision

d-pyl5qiwx = Mapped cell count

d-psq5vxbm = Has mapped cell count

d-325si2ag = Reported library area

d-4ldwfggs = Abc delay target ps

d-sdqy2njd = Has abc target

d-jxgfvcs7 = Manufacturable

d-d6eyywsn = Has manufacturable

d-hu5xloo5 = Synthetic coefficients

d-rmrnw3gy = Has synthetic coefficients

d-gvdrc4h2 = Synthetic activations

d-3uovqvrr = Has synthetic activations

d-6fqc7ll4 = Sample sha256

d-hsospyt4 = Sample receipt sha256

d-zenayb65 = Rtl source sha256

d-mbf2wgz7 = Real protocol

d-4ddzxjoe = C instrumentation

d-cbi2ze7u = External scalar comparisons

d-277q5ghg = Has external scalar comparisons

d-bdd52yao = Generic cell count

d-bsobd2xf = Has generic cell count

d-7bk6ofeo = Excluded metadata cells

d-3sohcddq = Has excluded metadata cells

d-5rnyzfkw = Compiler flags

d-u44pzow2 = Checks

d-zg5xxom6 = Tools

d-zxqh5fqc = Artifacts

d-4nv3clxq = Excludes

d-upp3twxb = Input digests

d-eryiyu4m = Primary sources

d-ljs2gw3t = Sources

d-ev5xwwya = Generic cells

d-yd5juegt = Operator

d-ytpaenff = Op original receipt sha256

d-67ramk2w = Op primary revision

d-l2hy4vng = Op compiler version

d-7mcamyri = Op compiler executable sha256

d-gbjecqi4 = Op compiler flags

d-s52ihrs2 = Op primary sources

d-el3wnlvr = Op local sources

d-vayzuyd3 = Op libraries

d-ph5z4g57 = Observations

d-3tunzd7f = Study

d-yu7n73kw = Study stage

d-o34vwu7q = Study original receipt sha256

d-czwcogej = Study producer sha256

d-iehv2mu5 = Study input identity sha256

d-idk7oh4r = Study expected comparison words

d-tietricm = Links

d-p4nm3blp = Variants

d-4zxln2bs = Flow

d-x4ibyx74 = Flow title

d-eti75aww = Flow input identity sha256

d-dj67bcy5 = Stages

d-6cz7yifm = Dependencies

d-ol3j2h7j = Subflows

d-s6kcjx6i-filter-status = Status

d-bmeexqpt = Project
    .doc = A workspace's chip design project: the evidence it imported and the structural operators a person sketched from it.
    .noun = { $count ->
        [one] project
       *[other] projects
    }
    .untitled = Chip Design

d-5vvat77h = Reports

d-lst2bswm = Verifications

d-qajzmqsz = Operators

d-ij6pw4z7 = Connections

d-lmh6n2cu = Selected
    .doc = What the window shows: `all`, `graph`, `report` or `receipt`.

d-iwadzlgx = Subject
    .doc = The digest of the report or receipt the window shows.

d-u7dhnpij = Node
    .doc = The operator whose own facts the window shows.

d-mppe3c5h = Output node
    .doc = The output a person chose to connect from.

d-ocvjm3oh = Output port

d-vrtmdnei = Zoom
    .doc = How far the graph is zoomed, in percent.

d-3oydn2gz = Stage
    .doc = The evidence stage the shown receipt holds open.

d-uuloznl2 = Macro
    .doc = The placement macro the shown stage holds open.

d-yl422q4s = Failure
    .doc = Why the last thing a person asked for was refused, in the engine's words.

d-2cb665aj = Notice
    .doc = What last happened, said plainly.

d-bmeexqpt-filter-zoom = Zoom

d-h6rtufxk = Import ask
    .noun = { $count ->
        [one] import ask
       *[other] import asks
    }

d-fmuu7ss2 = Json

d-yeycxwyn = Node ask
    .noun = { $count ->
        [one] node ask
       *[other] node asks
    }

d-zcgjq6hw = Id

d-xj4chp77 = Kind

d-qzhhc7sz = Shape

d-gpmyx3jk = Output width
    .doc = The matrix output width; zero when the operator has no matrix.

d-lcejfaxh = X

d-sjxnosz6 = Y

d-yeycxwyn-filter-kind = Kind

d-yeycxwyn-filter-output_width = Output width

d-yeycxwyn-filter-x = X

d-yeycxwyn-filter-y = Y

d-kycnmjh5 = Edge ask
    .noun = { $count ->
        [one] edge ask
       *[other] edge asks
    }

d-cem5h5og = From node

d-og4slduv = From port

d-esecrerr = To node

d-7ydvdfsk = To port

d-azqyhdbh = Graph ask
    .noun = { $count ->
        [one] graph ask
       *[other] graph asks
    }

d-6ydpdgb2 = Nodes

d-5sfjy4uo = Connections

d-fgztesqv = Outcome of
    .noun = { $count ->
        [one] outcome of
       *[other] outcome ofs
    }

d-bf2avrry = Digest

d-wfpvvvxh = Active

d-7ezbtipq = Reason

d-fgztesqv-filter-active = Active

d-fzjohe4t = Operator kind
    .doc = The structural operator kinds a design is built from.

d-rhhh2itq = Input

d-avno42l5 = Gate input

d-l3pmoeaz = Norm weight input

d-qki72q5u = Rms norm

d-6tlt753p = Apply norm weight

d-d5yprsar = Silu

d-bdcmtayu = Multiply

d-apt4dgg3 = Projection input

d-acuu4z4l = Matrix weight input

d-w23be4x4 = Linear projection

d-bjmzpc2n = Add

d-xtxvv5ey = Check result
    .doc = How a reported check ended.

d-sv2spyhp = Passed

d-6w5sjnjw = Failed

d-t2bem3bm = Run result
    .doc = How a reported run ended.

d-kmk626bt = Passed

d-556rg7nj = Failed

d-newd543u = Cancelled

d-xccazk3u = Cache kind
    .doc = Whether a storage report bounds its attention cache, or cannot say.

d-xtajlvle = Bound

d-owtits4w = Unsupported

d-ntuzz6km = Observation kind
    .doc = What a reported evidence stage observed.

d-lam2dzpr = None

d-gl2vdyxy = Cpu reference

d-5nrx2cub = Rtl simulation

d-xbq4pclg = Generic synthesis

d-e66d2bnp = Library physical

d-mk4umznw = Rms norm

d-n7efpvbz = Silu

d-uyr5cv7o = Gated delta

d-kbmdullp = Softmax

d-mp3q2dr6 = Comparison
    .doc = What a numerical comparison asserts.

d-abrpzhtn = Exact

d-q4gghfea = Characterization

d-7rrv34yb = Outcome
    .doc = How a reported RTL simulation ended.

d-nbmoug3l = Completed

d-ao76imbp = Expected refusal

d-ynbig6oe = Dependency kind
    .doc = What a reported evidence dependency carries.

d-mrubdlqf = Data

d-7xcqjq4q = Build

d-vdbwbp4i = Verification

d-6qnz2pt4 = Stage kind
    .doc = Which stage of an architecture comparison a receipt reports.

d-tv2begou = Prepare

d-2pnqkrjb = Numerical

d-5ggzqkan = Fault

d-ljmpko27 = Synthesis

d-xdwch5pa = Aggregate

d-jfmns64g = Orientation
    .doc = The reported macro placements' orientation.

d-4bx6xsie = R0

d-3x7sxkex = R90

d-b3l62otc = R180

d-ekckacui = R270

d-paas3xxn = Mx

d-zpmeg4e2 = My

d-plnfglts = Mxr90

d-cywro6el = Myr90

d-lz7sfooc = Import report
    .doc = Imports the engineering evidence a JSON file holds: an analytical storage report or a hardware verification receipt.
    .w-the-report-exceeds-the-1-mib = The report exceeds the 1 MiB import limit

d-lz7sfooc-file = File

d-2esuldj2 = Import analytical report
    .doc = Imports an analytical storage report from its JSON.

d-2esuldj2-document = Document

d-bp4izx7i = Import verification receipt
    .doc = Imports a hardware verification receipt from its JSON.

d-bp4izx7i-document = Document

d-aojvundi = Take in
    .doc = Takes in one imported document, whichever it is: the device checks it first, and what it holds reaches the project only once its own contract accepts it.

d-aojvundi-document = Document

d-rsfeeosl = Keep report
    .doc = Keeps one analytical storage report and the assumptions and scope it states.

d-rsfeeosl-digest = Digest

d-rsfeeosl-source = Source

d-rsfeeosl-facts = Facts

d-ptt4dxra = Keep receipt
    .doc = Keeps one hardware verification receipt: its scope, its checks, the identities it reports, and whichever of the three reported protocols it speaks.

d-ptt4dxra-digest = Digest

d-ptt4dxra-source = Source

d-ptt4dxra-facts = Facts

d-5uqijyq2 = Remove report
    .doc = Removes a report and everything it stated.

d-5uqijyq2-r = R

d-zzpfsfn4 = Remove verification receipt
    .doc = Removes a verification receipt and everything it stated.

d-zzpfsfn4-r = R

d-rhugesq6 = Operator graph
    .doc = Opens the operator graph.

d-sbus2usu = All reports
    .doc = Lists every report and receipt the project holds.

d-u7uj5e5v = Inspect report
    .doc = Shows one report.

d-u7uj5e5v-r = R

d-5hgiywn5 = Inspect verification receipt
    .doc = Shows one verification receipt.

d-5hgiywn5-r = R

d-lkubu4by = Inspect evidence
    .doc = Shows one stage of the receipt the window holds open.

d-lkubu4by-r = R

d-lkubu4by-s = S

d-fk62azrb = Inspect placement
    .doc = Shows one macro of the placement the window holds open.

d-fk62azrb-r = R

d-fk62azrb-m = M

d-bovpll5i = Open subflow
    .doc = Opens the receipt an evidence flow's subflow reference names, when its summary is imported.

d-bovpll5i-r = R

d-bovpll5i-s = S

d-xtme7y55 = Dismiss
    .doc = Shrugs off the line about the last refusal or success.

d-vmhkm6oz = Zoom the graph
    .doc = Zooms the operator graph in, out, or back to its whole extent.

d-vmhkm6oz-step = Step

d-xgsghcqx = Choose output
    .doc = Chooses the output a person connects from, naming the inputs it can feed.

d-xgsghcqx-op = Op

d-xgsghcqx-port = Port

d-mfra3i5f = Clear output
    .doc = Leaves the graph without a chosen output.

d-2nj2oywc = Inspect operator
    .doc = Shows one operator's own facts, or closes them.

d-2nj2oywc-op = Op

d-hhgkdr2m = Close operator

d-vlxtsdsp = Add operator
    .doc = Adds an operator of `kind` after the ones already in the graph, on a four-wide grid.

d-vlxtsdsp-name = Name

d-7woverrw = Add operator with a shape
    .doc = Adds one operator with exactly the shape, matrix width and place a caller gives it.

d-7woverrw-node = Node

d-7woverrw-kind = Kind

d-7woverrw-shape = Shape

d-7woverrw-output_width = Output width

d-7woverrw-x = X

d-7woverrw-y = Y

d-7ssomfsm = Set operator input shape
    .doc = Reshapes one operator.

d-7ssomfsm-op = Op

d-7ssomfsm-shape = Shape

d-64uig2eg = Apply shape
    .doc = Applies the shape a person typed for one operator, when it names one.

d-64uig2eg-op = Op

d-verclot2 = Apply output width
    .doc = Applies the matrix output width a person typed for one operator.

d-verclot2-op = Op

d-qixmmh5c = Tap macro
    .doc = Shows one placement macro of the stage the window holds open.

d-qixmmh5c-s = S

d-qixmmh5c-index = Index

d-hafquklm = Set matrix output width
    .doc = Sets the matrix output width of one operator.

d-hafquklm-op = Op

d-hafquklm-width = Width

d-ja53622w = Move operator
    .doc = Moves one operator to a place on the canvas.

d-ja53622w-op = Op

d-ja53622w-x = X

d-ja53622w-y = Y

d-jhu3cqwg = Remove operator
    .doc = Removes one operator, and every connection it held.

d-jhu3cqwg-op = Op

d-yiq7sfzp = Connect operators
    .doc = Connects one output to one input.

d-yiq7sfzp-from = From

d-yiq7sfzp-from_port = From port

d-yiq7sfzp-to = To

d-yiq7sfzp-to_port = To port

d-nz7gzmnw = Disconnect input
    .doc = Disconnects every connection into one input.

d-nz7gzmnw-op = Op

d-nz7gzmnw-port = Port

d-rcuv3d36 = Resolve graph
    .doc = Resolves the whole design again, so what the window shows is what the stored design makes.

d-mtz3jwsi = Apply graph
    .doc = Keeps the ports and connection outcomes the device answered, so the window draws and refuses by the design's own resolution.

d-mtz3jwsi-answer = Answer

d-kuhuzr64 = Inspect seen macro
    .doc = Shows one macro the person picked from the placement's own list.

d-kuhuzr64-r = R

d-kuhuzr64-s = S

d-kuhuzr64-m = M

d-2i4xepvu = Tap graph
    .doc = A press on the canvas: an operator's title opens its facts, an output is what the next input connects from, and an input connects or opens its operator.

d-2i4xepvu-index = Index

d-b53kihl2 = Import file~present

d-frkvs7gt = Window
    .w-chip-design = Chip Design
    .w-operator-graph = Operator graph
    .w-import-report = Import report
    .w-all-reports = All reports
    .w-that-could-not-be-done = That could not be done
    .w-dismiss = Dismiss
    .w-nothing-in-this-project-yet = Nothing in this project yet
    .w-reported-output-artifact-identities = Reported output artifact identities
    .w-not-reported = Not reported
    .w-exclusions-and-limitations = Exclusions and limitations
    .w-verification-exclusion = Verification exclusion
    .w-reported-numerical-observations = Reported numerical observations
    .w-remove-verification-receipt = Remove verification receipt
    .w-imported-analytical-storage-report-source-authenticity = Imported analytical storage report. Source authenticity and quantization quality are unverified.
    .w-reported-provenance = Reported provenance
    .w-model-repository = Model repository
    .w-reported-model-revision = Reported model revision
    .w-reported-config-sha-256 = Reported config SHA-256
    .w-imported-bytes-blake3 = Imported bytes BLAKE3
    .w-storage-assumptions = Storage assumptions
    .w-assumed-logical-parameters = Assumed logical parameters
    .w-assumed-weight-bits = Assumed weight bits
    .w-assumed-cache-bits = Assumed cache bits
    .w-assumed-context-tokens = Assumed context tokens
    .w-batch-size = Batch size
    .w-analytical-storage = Analytical storage
    .w-packed-weight-bytes = Packed weight bytes
    .w-cache-storage-unverified = Cache storage unverified
    .w-reported-full-attention-layers = Reported full-attention layers
    .w-reported-cache-elements = Reported cache elements
    .w-partial-cache-bytes = Partial cache bytes
    .w-excluded-from-cache = Excluded from cache
    .w-unestimated = Unestimated
    .w-report-limitation = Report limitation
    .w-verification-scope = Verification scope
    .w-internal-arithmetic-only-the-pinned-config = Internal arithmetic only. The pinned config is not reconstructed; hardware performance and coding quality are not measured.
    .w-remove-report = Remove report
    .w-node-identity = Node identity
    .w-fp32-shape = FP32 shape
    .w-input-width = Input width
    .w-normalization-width = Normalization width
    .w-position = Position
    .w-in = In
    .w-out = Out
    .w-apply-shape = Apply shape
    .w-apply-output-width = Apply output width
    .w-move-left = Move left
    .w-move-right = Move right
    .w-move-up = Move up
    .w-move-down = Move down
    .w-disconnect = Disconnect { $port_words }
    .w-remove-operator = Remove operator

d-i4fiqfpg = Places
    .w-operator-graph = Operator graph
    .w-all-reports = All reports
    .w-reports = Reports
    .w-verifications = Verifications

d-rnmryklx = Evidence list
    .w-nothing-in-this-project-yet = Nothing in this project yet
    .w-sketch-the-operators-in-the-operator = Sketch the operators in the operator graph, or import an engineering report (JSON) to read the evidence it records.
    .w-open-the-operator-graph = Open the operator graph
    .w-reports = Reports
    .w-verifications = Verifications
    .w-operators = Operators

d-xfrgv6rb = Report page
    .w-imported-analytical-storage-report-source-authenticity = Imported analytical storage report. Source authenticity and quantization quality are unverified.
    .w-reported-provenance = Reported provenance
    .w-model-repository = Model repository
    .w-reported-model-revision = Reported model revision
    .w-reported-config-sha-256 = Reported config SHA-256
    .w-imported-bytes-blake3 = Imported bytes BLAKE3
    .w-storage-assumptions = Storage assumptions
    .w-assumed-logical-parameters = Assumed logical parameters
    .w-assumed-weight-bits = Assumed weight bits
    .w-assumed-cache-bits = Assumed cache bits
    .w-assumed-context-tokens = Assumed context tokens
    .w-batch-size = Batch size
    .w-analytical-storage = Analytical storage
    .w-packed-weight-bytes = Packed weight bytes
    .w-cache-storage-unverified = Cache storage unverified
    .w-not-reported = Not reported
    .w-reported-full-attention-layers = Reported full-attention layers
    .w-reported-cache-elements = Reported cache elements
    .w-partial-cache-bytes = Partial cache bytes
    .w-excluded-from-cache = Excluded from cache
    .w-exclusions-and-limitations = Exclusions and limitations
    .w-unestimated = Unestimated
    .w-report-limitation = Report limitation
    .w-verification-scope = Verification scope
    .w-internal-arithmetic-only-the-pinned-config = Internal arithmetic only. The pinned config is not reconstructed; hardware performance and coding quality are not measured.
    .w-remove-report = Remove report

d-znn6bjwe = Receipt page
    .w-nothing-in-this-project-yet = Nothing in this project yet
    .w-reported-output-artifact-identities = Reported output artifact identities
    .w-not-reported = Not reported
    .w-exclusions-and-limitations = Exclusions and limitations
    .w-verification-exclusion = Verification exclusion
    .w-reported-numerical-observations = Reported numerical observations
    .w-remove-verification-receipt = Remove verification receipt

d-gynnzyfj = Receipt scope
    .w-reported-verification-scope = Reported verification scope
    .w-reported-run-result = Reported run result
    .w-evidence-class = Evidence class
    .w-failure-explanation = Failure explanation
    .w-failure-stage = Failure stage
    .w-reported-input-validation = Reported input validation
    .w-output-lanes = Output lanes
    .w-activation-group-size = Activation group size
    .w-reported-mapped-cell-count = Reported mapped cell count
    .w-sum-of-research-library-cell-areas = Sum of research library cell areas
    .w-abc-optimization-target-ps-not-measured = ABC optimization target (ps), not measured timing
    .w-process-scope = Process scope
    .w-nonmanufacturable-research-library = Nonmanufacturable research library
    .w-external-scalar-comparisons = External scalar comparisons
    .w-protocol-observation = Protocol observation
    .w-c-reference-instrumentation = C reference instrumentation
    .w-scalar-compiler-flags = Scalar compiler flags
    .w-reported-generic-cell-count = Reported generic cell count
    .w-excluded-metadata-cells = Excluded metadata cells
    .w-reported-experiment-stage = Reported experiment stage
    .w-declared-complete-output-words-per-variant = Declared complete output words per variant
    .w-reported-numerical-observation = Reported numerical observation

d-dlvirkwn = Receipt provenance
    .w-reported-provenance = Reported provenance
    .w-original-imported-bytes-blake3 = Original imported bytes BLAKE3
    .w-imported-bytes-sha-256 = Imported bytes SHA-256
    .w-not-reported = Not reported
    .w-declared-immutable-toolchain-image = Declared immutable toolchain image
    .w-reported-sample-manifest-sha-256 = Reported sample manifest SHA-256
    .w-reported-verifier-sha-256 = Reported verifier SHA-256
    .w-reported-artifact-validator-sha-256 = Reported artifact validator SHA-256
    .w-reported-mapper-sha-256 = Reported mapper SHA-256
    .w-reported-library-source-revision = Reported library source revision
    .w-reported-sample-digest = Reported sample digest
    .w-reported-sample-receipt-digest = Reported sample receipt digest
    .w-reported-rtl-source-digest = Reported RTL source digest
    .w-original-receipt-sha-256 = Original receipt SHA-256
    .w-reported-primary-source-revision = Reported primary source revision
    .w-reported-compiler-version = Reported compiler version
    .w-reported-tool-executable-sha-256 = Reported tool executable SHA-256
    .w-reported-compiler-flags = Reported compiler flags
    .w-reported-experiment-stage = Reported experiment stage
    .w-original-producer-receipt-sha-256 = Original producer receipt SHA-256
    .w-executed-producer-sha-256 = Executed producer SHA-256
    .w-shared-input-identity-sha-256 = Shared input identity SHA-256
    .w-tool-identities = Tool identities

d-ka2nnn5r = Observation row
    .w-reported-numerical-observation = Reported numerical observation
    .w-fp32-words-compared-exactly = { $profile }: { $fp32_comparisons } FP32 words compared exactly
    .w-inputs-silu-compared-exactly-exp-has = { $total_inputs } inputs: SiLU compared exactly; exp has { $exp_differences } differences, maximum { $max_exp_ulp } ULP
    .w-heads-tokens-width-attention-and-state = { $heads } heads, { $tokens } tokens, width { $width }: { $attention_words } attention and { $state_words } state words compared exactly
    .w-words-masked-differences-maximum-ulp = { $name }: { $width } words, { $masked } masked; { $differing_words } differences, maximum { $max_ulp } ULP

d-beykirjy = Study page
    .w-stage-links = Stage links
    .w-original-producer-receipt-sha-256 = Original producer receipt SHA-256
    .w-executed-producer-sha-256 = Executed producer SHA-256
    .w-shared-input-identity-sha-256 = Shared input identity SHA-256
    .w-cold-weight-programming-cycles = Cold weight programming cycles
    .w-activation-load-cycles = Activation load cycles
    .w-warm-execution-cycles = Warm execution cycles
    .w-accepted-packed-weight-bytes = Accepted packed weight bytes
    .w-accepted-block-descriptor-bytes = Accepted block descriptor bytes
    .w-activation-cache-reads = Activation cache reads
    .w-quantizer-packets = Quantizer packets
    .w-compared-output-words = Compared output words
    .w-reported-output-sha-256 = Reported output SHA-256
    .w-generic-logic-cells = Generic logic cells
    .w-activation-cache-bits = Activation cache bits
    .w-programmed-weight-storage-bits = Programmed weight storage bits
    .w-initialized-fixed-storage-bits = Initialized fixed storage bits
    .w-of = { $name } of { $identity }
    .w-word-width-bits = Word width (bits)
    .w-depth-words = Depth (words)
    .w-logical-storage-bits = Logical storage (bits)
    .w-read-ports = Read ports
    .w-clocked-read-ports = Clocked read ports
    .w-write-ports = Write ports
    .w-fully-initialized = Fully initialized
    .w-yes = Yes
    .w-no = No

d-j65hkz24 = Flow body
    .w-reported-evidence-flow = Reported evidence flow
    .w-reported-evidence-only = Reported evidence only
    .w-declared-input-identity-sha-256 = Declared input identity SHA-256
    .w-version-executable = Version { $version }, executable { $executable_sha256 }
    .w-reported-evidence-only-artifacts-are-not = Reported evidence only. Artifacts are not authenticated or executed. Library estimates are not silicon measurements, signoff, manufacturing approval, or model quality.
    .w-reported-stage-result = Reported stage result
    .w-observation = Observation
    .w-original-receipt-artifact = Original receipt artifact
    .w-declared-producer-sha-256 = Declared producer SHA-256
    .w-scope-limitation = Scope limitation
    .w-declared-dependency = Declared dependency
    .w-feeds-artifact = { $from } feeds { $to }, { $dependency_words }, artifact { $artifact }
    .w-reported-macro-placement = Reported macro placement
    .w-macro-footprints-only-other-instances-are = Macro footprints only; other instances are omitted. Reported geometry does not establish layout authentication or signoff. Coordinates are in micrometers, with the physical origin at the lower left.
    .w-total-reported-instances = Total reported instances
    .w-instances-omitted-from-this-view = Instances omitted from this view
    .w-die-bounds-m = Die bounds (µm)
    .w-cell-master = Cell master
    .w-macro-bounds-m = Macro bounds (µm)
    .w-orientation = Orientation
    .w-placement-status = Placement status
    .w-original-artifact-references = Original artifact references

d-hus44a4k = Evidence stage card

d-sze4ff2m = Evidence subflow card
    .w-subflow-reference = Subflow reference

d-n622kuhe = Stage page
    .w-reported-stage-result = Reported stage result
    .w-observation = Observation
    .w-original-receipt-artifact = Original receipt artifact
    .w-declared-producer-sha-256 = Declared producer SHA-256
    .w-declared-input-identity-sha-256 = Declared input identity SHA-256
    .w-scope-limitation = Scope limitation
    .w-declared-dependency = Declared dependency
    .w-feeds-artifact = { $from } feeds { $to }, { $dependency_words }, artifact { $artifact }
    .w-reported-macro-placement = Reported macro placement
    .w-macro-footprints-only-other-instances-are = Macro footprints only; other instances are omitted. Reported geometry does not establish layout authentication or signoff. Coordinates are in micrometers, with the physical origin at the lower left.
    .w-total-reported-instances = Total reported instances
    .w-instances-omitted-from-this-view = Instances omitted from this view
    .w-die-bounds-m = Die bounds (µm)
    .w-cell-master = Cell master
    .w-macro-bounds-m = Macro bounds (µm)
    .w-orientation = Orientation
    .w-placement-status = Placement status

d-erao3yv4 = Macro page
    .w-cell-master = Cell master
    .w-macro-bounds-m = Macro bounds (µm)
    .w-orientation = Orientation
    .w-placement-status = Placement status

d-2jgrmrzq = Subflow page
    .w-subflow-reference = Subflow reference
    .w-relative-artifact-reference = Relative artifact reference
    .w-declared-artifact-sha-256 = Declared artifact SHA-256
    .w-not-reported = Not reported
    .w-open-imported-subflow = Open imported subflow
    .w-referenced-summary-has-not-been-imported = Referenced summary has not been imported. No execution claim is made for this reference.

d-op24aqz3 = Operator page
    .w-node-identity = Node identity
    .w-fp32-shape = FP32 shape
    .w-input-width = Input width
    .w-normalization-width = Normalization width
    .w-position = Position
    .w-in = In
    .w-out = Out
    .w-apply-shape = Apply shape
    .w-apply-output-width = Apply output width
    .w-move-left = Move left
    .w-move-right = Move right
    .w-move-up = Move up
    .w-move-down = Move down
    .w-disconnect = Disconnect { $port_words }
    .w-remove-operator = Remove operator

d-lcswi4co = Graph body
    .w-add-operator = Add operator
    .w-operator = Operator
    .w-zoom-out = Zoom out
    .w-fit-to-window = Fit to window
    .w-zoom-in = Zoom in
    .w-structural-design-only-this-graph-does = Structural design only. This graph does not run or qualify hardware.
    .w-cancel = Cancel
    .w-to-connect-two-operators-choose-an = To connect two operators, choose an output, then an input it can feed.
    .w-no-operators-yet = No operators yet
    .w-add-one-to-start-designing = Add one to start designing.
    .w-the-structural-operator-design = The structural operator design
