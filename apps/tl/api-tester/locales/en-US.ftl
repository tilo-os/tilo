# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-api-tester = API Tester

d-so-tilo-api-tester-keywords = postman, insomnia, rest, http, requests, endpoint, graphql, api, client, debug, test

d-awdfqlwu = Row
    .doc = One key and value of a request: a query parameter, a header or a form field.
    .noun = { $count ->
        [one] row
       *[other] rows
    }
    .untitled = New row

d-gmr3auev = Key

d-5i27urp3 = Value

d-vgdlc7cu = Enabled
    .doc = Included: a row switched off stays listed and is not sent.

d-awdfqlwu-filter-enabled = Enabled

d-jqblcgrx = Credentials
    .doc = What a request signs in with.
    .noun = { $count ->
        [one] credential
       *[other] credentials
    }

d-wu5jqin3 = Scheme

d-eimiw54w = Username
    .doc = The user name of basic authentication: public, as the name of the account is.

d-bq35bust = Header
    .doc = The header an API key travels in.

d-doie7hps = Reference
    .doc = The device-local name of the credential.

d-jqblcgrx-filter-scheme = Scheme

d-qpu6dw3a = Request
    .doc = One request, saved in a collection or being built.
    .noun = { $count ->
        [one] request
       *[other] requests
    }
    .untitled = New request

d-zerzgb7o = Name

d-plszv7l4 = Method

d-2myyqmpp = Url

d-d26dsrkb = Params

d-sp2vfioq = Headers

d-oubruxyi = Auth

d-26dsckeq = Body mode

d-ejporjru = Body
    .doc = The body as typed: JSON, raw text or a template.

d-2koo3omi = Form

d-qpu6dw3a-filter-method = Method

d-qpu6dw3a-filter-auth = Auth

d-qpu6dw3a-filter-body_mode = Body mode

d-cgnrf3nr = Answer
    .doc = What a service answered.
    .noun = { $count ->
        [one] answer
       *[other] answers
    }
    .untitled = Response

d-cmtsv52e = Status

d-s2rpes4w = Status text

d-bmbte2qq = Duration ms

d-7xxkbrmd = Size bytes

d-lfculcqs = Size exact
    .doc = Whether `size_bytes` is the body's size, rather than how much of an undeclared body arrived before the download stopped.

d-52brcezl = Shown bytes
    .doc = How many of the body's bytes the view shows.

d-dczl7ahz = Binary
    .doc = Whether the body is binary, so the view shows a hex dump of its start.

d-ue25vzen = Content type

d-nxs5lmpc = Body
    .doc = The body: as received, or a hex dump when it is binary.

d-ryqihqco = Pretty
    .doc = The body laid out as JSON reads, for the pretty view; the body as received otherwise.

d-anvyklqi = Headers

d-cgnrf3nr-filter-status = Status

d-cgnrf3nr-filter-duration_ms = Duration ms

d-cgnrf3nr-filter-size_bytes = Size bytes

d-cgnrf3nr-filter-size_exact = Size exact

d-cgnrf3nr-filter-shown_bytes = Shown bytes

d-cgnrf3nr-filter-binary = Binary

d-osuueta7 = Answer header
    .doc = One header a service answered with.
    .noun = { $count ->
        [one] answer header
       *[other] answer headers
    }
    .untitled = Header

d-q5ydbtzd = Name

d-vyjpofsi = Value

d-mxmrgmfk = Environment
    .doc = A set of `{"{"}{ $variable }{"}"}` substitutions, resolved into a request just before it is sent.
    .noun = { $count ->
        [one] environment
       *[other] environments
    }
    .untitled = Untitled environment

d-2kunn2pf = Name

d-yhhzdhos = Variables

d-oldlt6c7 = Variable
    .doc = One variable of an environment.
    .noun = { $count ->
        [one] variable
       *[other] variables
    }
    .untitled = variable

d-i3e2wno7 = Key

d-qqgvzyfe = Value

d-7bxonldr = Enabled

d-og7vyhyy = Secret
    .doc = Whether the value is a credential kept on each device.

d-v6jf7adc = Reference
    .doc = The device-local name of a secret value.

d-oldlt6c7-filter-enabled = Enabled

d-oldlt6c7-filter-secret = Secret

d-rghtwwim = Sent
    .doc = One request that was sent, as history keeps it.
    .noun = { $count ->
        [one] sent
       *[other] sents
    }
    .untitled = Sent request

d-ikluhyoi = Route
    .doc = The method and the address without credentials, query values or fragment, so it can name the entry anywhere.

d-fwwi364f = Method

d-w7aoohfv = Url

d-7pfct73j = Status

d-cno6gxwc = Duration ms

d-irpsjk53 = At

d-4mnpilrd = Replay
    .doc = The request as it was authored when it was sent, with variables as typed and never a credential, so reopening the entry brings the whole request back.

d-rghtwwim-filter-method = Method

d-rghtwwim-filter-status = Status

d-rghtwwim-filter-duration_ms = Duration ms

d-rghtwwim-filter-at = At

d-tkaqpxy4 = Saved collection
    .doc = What a saved collection of requests is called.
    .noun = { $count ->
        [one] saved collection
       *[other] saved collections
    }
    .untitled = Untitled collection

d-6bygepf4 = Name

d-vghsgjnl = Requests

d-vyss2ux2 = Workspace
    .doc = The workspace's API tester: collections of requests, the environments that fill their variables, the request being built, and what was sent.
    .noun = { $count ->
        [one] workspace
       *[other] workspaces
    }

d-bqk5mejh = Collections

d-7idvazp5 = Environments

d-bnuffmnw = Active environment
    .doc = Whose variables resolve into what is sent.

d-t7bbwzg4 = Builder
    .doc = The workspace's request builder: the request being typed, overwritten when another is opened.

d-oela3gry = Opened
    .doc = The saved request the builder holds a copy of.

d-yughiaxm = History
    .doc = What was sent, newest first.

d-ylmn6amm = Sending
    .doc = A request is on its way; `job` names it on this device.

d-lzzl5iuz = Job

d-be7aufil = Sent replay
    .doc = The request as authored when it went out, kept for the history entry it makes.

d-6msktvdz = Answer
    .doc = What the service answered, once it did.

d-bhgazpm3 = Answered
    .doc = Whether the response pane shows an answer or a failure at all.

d-674n5s6g = Failure
    .doc = Why the last send failed, in the device's words.

d-si6y5pgr = Notice
    .doc = One line under the response about the last copy.

d-vyss2ux2-filter-active_environment = Active environment

d-vyss2ux2-filter-builder = Builder

d-vyss2ux2-filter-opened = Opened

d-vyss2ux2-filter-sending = Sending

d-vyss2ux2-filter-answer = Answer

d-vyss2ux2-filter-answered = Answered

d-suqu77re = Send ask
    .doc = What the app asks its engine to send: the request as typed, its variables, and the reference that names its sign-in on this device.
    .noun = { $count ->
        [one] send ask
       *[other] send asks
    }

d-natoiy5b = Method

d-xg26wm2t = Url

d-52ykdq3w = Params

d-2g54j6fl = Headers

d-5hvbl66x = Vars
    .doc = Each variable as name, value and reference, a reference only for a secret.

d-o4mypkv3 = Auth scheme

d-wgtleqed = Auth username

d-5wzmu546 = Auth header

d-6wq4j3vq = Auth reference

d-dnf4fjhy = Body mode

d-um3yzxeu = Body

d-m7c3owzv = Form

d-gjneojn5 = Keep ask
    .doc = What the app asks its device to keep.
    .noun = { $count ->
        [one] keep ask
       *[other] keep asks
    }

d-qxcspakn = Value

d-y337lll5 = Forget ask
    .doc = What is taken away again.
    .noun = { $count ->
        [one] forget ask
       *[other] forget asks
    }

d-42ltyzyi = Reference

d-anj6vtfw = Kept credential
    .doc = The reference a kept value answers with.
    .noun = { $count ->
        [one] kept credential
       *[other] kept credentials
    }

d-haaxlsd4 = Reference

d-77rawkj7 = Send state
    .doc = How far a request on its way has got: its state, and what it answered as one JSON text.
    .noun = { $count ->
        [one] send state
       *[other] send states
    }

d-tgafs5zj = State

d-vvonqua3 = Answer
    .doc = The answer as JSON text, empty while the request is on its way.

d-or6k3lr7 = Error

d-q7tlie2e = Answered
    .doc = What a service answered, as the engine writes it.
    .noun = { $count ->
        [one] answered
       *[other] answereds
    }

d-s4gqhsru = Status

d-kqbtxrg4 = Status text

d-3ffoqs7c = Duration ms

d-aciatrpq = Size bytes

d-yjlhfc23 = Size exact

d-n36ppxll = Shown bytes

d-f4fheroi = Binary

d-loetbxur = Content type

d-5c64ziqe = Body

d-agnxdiuh = Headers

d-q7tlie2e-filter-status = Status

d-q7tlie2e-filter-duration_ms = Duration ms

d-q7tlie2e-filter-size_bytes = Size bytes

d-q7tlie2e-filter-size_exact = Size exact

d-q7tlie2e-filter-shown_bytes = Shown bytes

d-q7tlie2e-filter-binary = Binary

d-4vsac7p2 = Replay
    .doc = A request as it was authored, for a history entry to bring back.
    .noun = { $count ->
        [one] replay
       *[other] replays
    }

d-7fy3fg22 = Name

d-woxdrlbr = Method

d-uehndmlh = Url

d-dw7wgcqy = Params

d-oxqptjsf = Headers

d-i66no2tx = Form

d-c7sufjee = Scheme

d-c4lbngho = Username

d-p3sqgmt6 = Header

d-wczlgfhd = Reference

d-x5jj7sdv = Body mode

d-5ez7t24e = Body

d-6pzmh6yh = Http method
    .doc = The methods a request is sent with, in the order the chooser lists them.

d-ccq7wyrs = Get

d-rudxo7g7 = Post

d-dgce4rjr = Put

d-yfhoi6zh = Patch

d-hs76ooyb = Delete

d-swq6ve3f = Head

d-fsyk6dx3 = Options

d-egb25ohn = Auth scheme
    .doc = How a request signs in.

d-ylg4gzt5 = None

d-hvxjosf7 = Bearer

d-nrg462nj = Basic

d-p2c6ixlq = Api key

d-wfohrpkl = Body mode
    .doc = What a request's body is.

d-wqsqfsd2 = None

d-kdrofv7x = Json

d-dqbyfwmb = Form

d-rfzohfpq = Raw

d-tbiad6gq = New request
    .doc = Starts a blank request in the builder, leaving the one it held.
    .did = started a request

d-tsedmdm4 = New collection
    .doc = Creates a collection to keep requests in.
    .w-a-collection-needs-a-name = A collection needs a name

d-tsedmdm4-name = Name

d-ho2h6gbu = Rename collection
    .doc = Renames a collection.
    .w-a-collection-needs-a-name = A collection needs a name

d-ho2h6gbu-name = Name

d-g2553ryv = Delete collection
    .doc = Deletes a collection and the requests saved in it.
    .w-that-collection-is-gone = That collection is gone

d-g2553ryv-c = C

d-5dcdkj3j = Add request
    .doc = Adds a request to a collection.

d-5dcdkj3j-name = Name

d-rs63hvbv = Save request
    .doc = Saves the request in the builder into a collection.
    .did = saved a request

d-rs63hvbv-c = C

d-tdten6rl = Fill from
    .doc = Copies everything a request holds into another one.

d-tdten6rl-src = Src

d-rgqkewiq = Open request
    .doc = Opens a saved request in the builder.
    .did = opened a request

d-rgqkewiq-r = R

d-qallswrm = Rename request
    .doc = Renames the request in the builder.
    .w-a-request-needs-a-name = A request needs a name

d-qallswrm-name = Name

d-irb2ga4y = Delete request
    .doc = Deletes a saved request.

d-irb2ga4y-r = R

d-sjsr23pm = Delete the saved request
    .doc = Deletes the saved request the builder holds a copy of.

d-p2234n73 = Add parameter
    .doc = Adds a query parameter to the request in the builder.

d-2ewontfg = Delete parameter
    .doc = Takes a query parameter off the request in the builder.

d-2ewontfg-entry = Entry

d-f6z5aeyu = Add header
    .doc = Adds a header to the request in the builder.

d-mygqj473 = Delete header
    .doc = Takes a header off the request in the builder.

d-mygqj473-entry = Entry

d-f7x7k7up = Add form field
    .doc = Adds a form field to the request in the builder.

d-vrocssvn = Delete form field
    .doc = Takes a form field off the request in the builder.

d-vrocssvn-entry = Entry

d-lfxyfzp2 = New environment
    .doc = Creates an environment and starts using it.
    .w-an-environment-needs-a-name = An environment needs a name

d-lfxyfzp2-name = Name

d-74wev3q6 = Rename environment
    .doc = Renames an environment.
    .w-an-environment-needs-a-name = An environment needs a name

d-74wev3q6-name = Name

d-6akuoupy = Delete environment
    .doc = Deletes an environment and the variables it held.

d-6akuoupy-e = E

d-tc2ujjsw = Delete the environment
    .doc = Deletes the environment in use.

d-khft6shk = Use environment
    .doc = Switches the environment whose variables fill what is sent.
    .did = switched environment
    .w-that-environment-is-gone = That environment is gone

d-khft6shk-e = E

d-tksdzarq = No environment
    .doc = Sends with no environment: every `{"{"}{ $variable }{"}"}` stays as typed.
    .did = stopped using an environment

d-cg6szno5 = Add variable
    .doc = Adds a variable to an environment.
    .w-a-variable-needs-a-key = A variable needs a key

d-cg6szno5-key = Key

d-cg6szno5-secret = Secret

d-6ga5m6wc = Delete variable
    .doc = Takes a variable off its environment, and its value off this device.

d-6ga5m6wc-v = V

d-e54vruws = Store sign-in
    .doc = Keeps the value typed for the request's sign-in where the workspace cannot read it, and remembers only the reference.
    .did = stored a sign-in
    .w-type-the-credential-first = Type the credential first

d-e54vruws-value = Value

d-c53ttbvc = Store value
    .doc = Keeps the value typed for a secret variable where the workspace cannot read it.
    .did = stored a value
    .w-this-variable-s-value-is-shared = This variable's value is shared; edit it in its field
    .w-type-the-value-first = Type the value first

d-c53ttbvc-value = Value

d-pop77kab = Forget value
    .doc = Takes a stored value off this device.

d-pop77kab-reference = Reference

d-rlv4ljnh = Set scheme
    .doc = Changes the scheme, taking the credential of the one before off this device.

d-rlv4ljnh-scheme = Scheme

d-b4rb4yd6 = Forget credential
    .doc = Takes the request's sign-in off this device.

d-w3dgdots = Send
    .doc = Sends what the builder holds to the address it names, in the environment in use.
    .did = sent a request
    .w-a-request-is-already-on-its = A request is already on its way
    .w-enter-a-url-to-send-the = Enter a URL to send the request to

d-gw3xhdma = Cancel request
    .doc = Stops waiting for the service to answer.

d-26aerujs = Send ended
    .doc = The request ended: the window keeps the answer it brought, or says why it did not come.

d-26aerujs-j = J

d-gfuhdzbs = Open sent
    .doc = Opens a request that was sent before, with what it held when it went out.
    .did = opened a sent request

d-gfuhdzbs-s = S

d-zxrevged = Save sent request
    .doc = Saves a sent request as a request in a collection, named by its route unless it had a name.

d-zxrevged-s = S

d-zxrevged-c = C

d-y3twmqop = Forget sent request
    .doc = Forgets a sent request.

d-y3twmqop-s = S

d-m7mspkdz = Copy body
    .doc = Copies the response body, exactly as the response pane shows it.
    .w-there-is-no-response-to-copy = There is no response to copy yet

d-w7v6ggv4 = Copy headers
    .doc = Copies every response header, one `Name: value` per line.
    .w-there-is-no-response-to-copy = There is no response to copy yet

d-csun3fww = Copy request
    .doc = Copies the request as authored, with its `{"{"}{ $variables }{"}"}` and never a credential.

d-mfnmmp63 = Dismiss notice
    .doc = Dismisses the line under the response.

d-obhk4ydu = Keep answer
    .doc = Keeps the answer in the workspace, so the window and an agent read the same one.

d-obhk4ydu-answer = Answer

d-54zyn23o = Record sent
    .doc = Adds the request that just came back to the workspace's history, newest first.

d-54zyn23o-answer = Answer

d-umcbmlvs = Set body mode

d-umcbmlvs-mode = Mode

d-l6jtvmn5 = New collection~present

d-wle7pvzd = Rename collection~present

d-tmkehrdk = Delete collection~present

d-wn5cuc2q = Rename draft~present

d-xxkpfuka = New environment~present

d-prn3oxjx = Rename environment~present

d-ubm223na = Use environment~present

d-jhmm6cq2 = Add variable~present

d-idikzlq5 = Keep credential~present

d-4pwx6lsh = Keep variable~present

d-5bgid526 = Send~present

d-jakzld6e = Copy body~present

d-fmqxk3ta = Copy headers~present

d-6z4z4ef7 = Tester
    .w-api-tester = API Tester
    .w-new-collection = New collection
    .w-no-environment = No environment
    .w-new-environment = New environment…
    .w-rename-environment = Rename environment…
    .w-variables = Variables…
    .w-add-variable = Add variable…
    .w-delete-the-variable = Delete the variable { $key }
    .w-set-the-value-of = Set the value of { $key }…
    .w-delete-environment = Delete environment…
    .w-rename-the-collection = Rename the collection { $name }…
    .w-delete-the-collection = Delete the collection { $name }…
    .w-collections = Collections
    .w-no-collections-yet = No collections yet
    .w-history = History
    .w-cancel-request = Cancel request
    .w-send = Send
    .w-save-request = Save request
    .w-the-sent-request-into = The sent request into { $name }
    .w-new-collection-2 = New collection…
    .w-request-actions = Request actions
    .w-new-request = New request
    .w-rename-request = Rename request…
    .w-delete-the-saved-request = Delete the saved request…
    .w-copy-request = Copy request
    .w-forget-the-sent-request = Forget the sent request…
    .w-request = Request
    .w-params = Params
    .w-headers = Headers
    .w-auth = Auth
    .w-body = Body
    .w-query-parameters = Query parameters
    .w-add-parameter = Add parameter
    .w-no-query-parameters = No query parameters
    .w-add-header = Add header
    .w-no-headers = No headers
    .w-no-response-yet = No response yet
    .w-send-the-request-to-see-what = Send the request to see what comes back.
    .w-waiting-for-the-server-to-respond = Waiting for the server to respond
    .w-couldn-t-complete-the-request = Couldn't complete the request
    .w-try-again = Try again
    .w-dismiss = Dismiss
    .w-copy-body = Copy body
    .w-copy-headers = Copy headers
    .w-response = Response
    .w-pretty = Pretty
    .w-raw = Raw
    .w-create = Create
    .w-cancel = Cancel
    .w-collection-name = Collection name
    .w-rename-collection = Rename collection
    .w-rename = Rename
    .w-delete-the-collection-2 = Delete the collection?
    .w-delete = Delete
    .w-new-environment-2 = New environment
    .w-environment-name = Environment name
    .w-rename-environment-2 = Rename environment
    .w-delete-the-environment = Delete the environment?
    .w-rename-request-2 = Rename request
    .w-request-name = Request name
    .w-delete-the-saved-request-2 = Delete the saved request?
    .w-the-saved-request-is-deleted-this = The saved request is deleted. This cannot be undone.
    .w-add-variable-2 = Add variable
    .w-variable-key = Variable key
    .w-its-value-is-a-credential-kept = Its value is a credential kept on each device
    .w-variables-2 = Variables
    .w-done = Done
    .w-use-this-variable = Use this variable
    .w-no-value-kept = No value kept
    .w-kept-on-this-device = Kept on this device
    .w-set-the-value = Set the value…
    .w-variable-value = Variable value
    .w-delete-variable = Delete variable
    .w-no-variables-yet = No variables yet
    .w-set-the-value-2 = Set the value
    .w-store = Store
    .w-value = Value

d-a6fjnueq = Url bar
    .w-method = Method
    .w-enter-a-url = Enter a URL

d-u5vthwsc = Parameter row
    .w-use-this-parameter = Use this parameter
    .w-key = Key
    .w-value = Value
    .w-delete-parameter = Delete parameter

d-xira45uc = Header row
    .w-use-this-header = Use this header
    .w-header-name = Header name
    .w-value = Value
    .w-delete-header = Delete header

d-xyfp5r4l = Form row
    .w-use-this-field = Use this field
    .w-key = Key
    .w-value = Value
    .w-delete-field = Delete field

d-t7usmncz = Auth panel
    .w-authentication = Authentication
    .w-scheme = Scheme
    .w-no-auth = No Auth
    .w-bearer-token = Bearer Token
    .w-basic-auth = Basic Auth
    .w-api-key = API Key
    .w-this-request-does-not-use-authentication = This request does not use authentication.
    .w-a-sign-in-is-kept-on = A sign-in is kept on this device.
    .w-no-sign-in-is-kept-on = No sign-in is kept on this device yet.
    .w-username = Username
    .w-header-name = Header name
    .w-forget-sign-in = Forget sign-in
    .w-set-the-sign-in = Set the sign-in
    .w-store = Store
    .w-cancel = Cancel

d-dvg27g6l = Body panel
    .w-body = Body
    .w-none = None
    .w-json = JSON
    .w-form = Form
    .w-raw = Raw
    .w-this-request-does-not-have-a = This request does not have a body.
    .w-add-form-field = Add form field
    .w-no-form-fields = No form fields
    .w-the-body = The body

d-3n3wrhwy = Response headers
    .w-response-headers = Response headers
    .w-name = Name
    .w-value = Value
