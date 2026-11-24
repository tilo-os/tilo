# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-browser = Browser

d-so-tilo-browser-keywords = web, internet, surf, website, url, www, web browser, chrome, safari, edge, firefox, browser, browsen, webseite, internetseite

d-lrkgwd2c = Bookmark
    .doc = A site someone saved: where it is and what it is called.
    .noun = { $count ->
        [one] bookmark
       *[other] bookmarks
    }

d-44buv5tt = Url

d-b3zhsydm = Title

d-ehmpzm3r = Folder
    .doc = The folder the bookmark stands in, as a path in the person's own library (`Work/Reading`): empty for one at the top.

d-em2t5s5a = Visit
    .doc = A page the person visited: where it is, what it was called when they last read it, and when that was.
    .noun = { $count ->
        [one] visit
       *[other] visits
    }

d-k73mxvmp = Url

d-qy6vtgjc = Title

d-2yoxl6hx = Last
    .doc = When the person last opened it, on their clock.

d-eruwucur = Count
    .doc = How many times a window opened it.

d-siihm6ib = Imported visit
    .doc = A page taken in from another browser's history, kept beside the visits this browser made and counted apart, so importing the same history twice keeps one page.
    .noun = { $count ->
        [one] imported visit
       *[other] imported visits
    }

d-my4dhx2s = Url

d-43vnrbh6 = Title

d-avuwnnrf = Last

d-srpuiw6c = Count

d-hkdk5jeo = Remembered form
    .doc = A site's remembered form: what it asked and what it called each question, never an answer.
    .noun = { $count ->
        [one] remembered form
       *[other] remembered forms
    }

d-3wdvc5zt = Origin
    .doc = The site's origin (`https://example.org`), as tracker protection knows it too.

d-ksw3jnev = Signature
    .doc = The form's shape, which names the same form on a later visit and stops naming it once the form asks something else.

d-vdjubgjc = Label
    .doc = What the form is called: its own legend, or the page it stands on.

d-6atmgq6f = Pages
    .doc = The pages of the site the form was read on.

d-znuzitns = Seen
    .doc = How many times the form was read.

d-kexnvpi2 = Read at
    .doc = When it was last read, on the person's clock.

d-jalszv2p = Questions

d-oizi2wqk = Question
    .doc = One question of a remembered form: what it is called, what it asks for and the answers it offers.
    .noun = { $count ->
        [one] question
       *[other] questions
    }

d-2wlmb5rp = Name

d-z4pmfk6p = Answer
    .doc = What kind of answer it takes: `text`, `email`, `tel`, `number`, `url`, `password`, `choice`, `choices`, `file`, `date` or `checkbox`.

d-mttccz7q = Label
    .doc = What the question asks, as the page labels it.

d-yk2bhzup = Choices

d-gmhi7gfy = Required

d-oizi2wqk-filter-required = Required

d-b7ln6koa = Address
    .doc = An address the person fills forms with, offered where a form asks for one.
    .noun = { $count ->
        [one] address
       *[other] addresses
    }

d-fnyioh2y = Label

d-3ewzn2z4 = Full name

d-n2u44pv5 = Given name

d-lgputt7y = Family name

d-plkha23f = Street

d-pwnlxkv7 = City

d-g7xpip4v = Region

d-kj2h5fbf = Postal code

d-iciyfupp = Country

d-l5vuna4b = Page
    .doc = A page open in a window: its address, what it is called, and what was done in it, newest last.
    .noun = { $count ->
        [one] page
       *[other] pages
    }

d-hf6q2oo4 = Url

d-tyhunk32 = Title

d-4nlkcuks = Paused
    .doc = History is paused in this page's window: pages opened there are not added to the person's history.

d-dedg3fev = Steps
    .doc = What was done in the page, newest last.

d-mk755sf3 = Dialog
    .doc = What the page's own script asks, and the menu it showed, as its window last read them.

d-tveuvbse = Menu

d-o2occ235 = Downloads
    .doc = The files the page downloads, as its window last read them: engine state, never the page's own record.

d-eg4w4fow = Muted
    .doc = Whether the page's sound is muted.

d-252payib = Site
    .doc = The site's connection, permissions and stored data, as its window last read them.

d-ctm6ma5g = Back
    .doc = The pages before this one and the ones after it, as the engine last answered the window's request for them.

d-znzxwmxa = Forward

d-eiqyiw53 = Loading
    .doc = How far the page has loaded, what it is zoomed to, and where the find bar stands.

d-xu2jvlwq = Progress

d-5gspmng7 = Zoom level

d-j5emshom = Find at

d-n6y73q7c = Find count

d-j257iaef = Page step
    .doc = One thing done in a page: who asked, what was done, what it acted on and how it ended.
    .noun = { $count ->
        [one] page step
       *[other] page steps
    }

d-cpjp36fw = At
    .doc = When it was done, in milliseconds since the page opened.

d-4mcq24cu = Actor
    .doc = Who asked: a person, an agent, a script.

d-g4lc2ulh = Device
    .doc = The device the actor worked from: empty for this one.

d-ouuwq4an = Device name

d-7qikcw5o = Operation
    .doc = What was done, as the page's actions name it.

d-olgrtknj = Ok

d-2mfaldtw = Code

d-grbghvzm = Message

d-vlqrueqe = Url
    .doc = The address it acted on, and the one it went to, for a step that navigated.

d-x3xrjcbr = To url

d-ta3jd45o = Role
    .doc = The element it acted on, by its role and its name.

d-kmz5bb2s = Element

d-g67lnb6k = Text
    .doc = The text it wrote, or the choices it made.

d-ws5snl25 = Values

d-j257iaef-filter-at = At

d-j257iaef-filter-actor = Actor

d-j257iaef-filter-ok = Ok

d-o3koqzgj = Dialog ask
    .doc = A question the page's own script asks: an alert, a confirmation, a prompt or a page leaving.
    .noun = { $count ->
        [one] dialog ask
       *[other] dialog asks
    }

d-trchguwk = Request
    .doc = The engine's id for the question, which the answer names.

d-plkldhkl = Kind
    .doc = `alert`, `confirm`, `prompt` or `leave`.

d-ruephqmr = Origin

d-asepg6kx = Message

d-4v2f3ua3 = Default text

d-o3koqzgj-filter-request = Request

d-3hxkb5pc = Menu ask
    .doc = The menu the page showed: its rows, and where it showed them.
    .noun = { $count ->
        [one] menu ask
       *[other] menu asks
    }

d-hjqv4o64 = Request

d-f2gbwl6v = Link

d-6b3vpys4 = Items

d-3hxkb5pc-filter-request = Request

d-hmusuedw = Menu row
    .doc = One row of the page's menu: a command, or a rule between groups.
    .noun = { $count ->
        [one] menu row
       *[other] menu rows
    }

d-kybyncey = Command

d-cgfpo6b7 = Label

d-hn5vxavq = Disabled

d-qmekhn2b = Checked

d-67scpsna = Separator

d-hmusuedw-filter-command = Command

d-hmusuedw-filter-disabled = Disabled

d-hmusuedw-filter-checked = Checked

d-hmusuedw-filter-separator = Separator

d-cjw2xcfs = Download
    .doc = One file a page downloads: where it comes from, where it went, and how far it has come.
    .noun = { $count ->
        [one] download
       *[other] downloads
    }

d-anfisgsq = Transfer
    .doc = The engine's number for the transfer, which what the person does to it names.

d-in525zqp = State
    .doc = `awaiting`, `downloading`, `completed`, `failed` or `cancelled`.

d-wz6wi2p3 = Name

d-z4gzfvzw = Url

d-feka5n4y = Path
    .doc = Where it was saved, once it is complete.

d-kubpqqew = Retryable
    .doc = Whether the engine can start it again after it failed.

d-cokrfftg = Received
    .doc = How many bytes have come, of how many the engine knows: none when it does not.

d-pbavpdyk = Total

d-wfaksaa6 = Error

d-qeovjxlz = Site info
    .doc = What a site's connection, its permissions and its stored data are, as the page's window last read them: engine state, refreshed when the window reads the page.
    .noun = { $count ->
        [one] site info
       *[other] site infos
    }

d-hrkhpkbd = Origin

d-scje2sv3 = Generation
    .doc = The document's generation the engine answered for: a site action names it, so a command meant for this page never lands on one that came after.

d-lyopffwi = Blocked
    .doc = How many requests in this page tracker protection blocked or replaced.

d-az2v2pr3 = Protected
    .doc = Whether tracker protection is on for this site.

d-ageqt2tb = Security
    .doc = `secure`, `mixed`, `insecure` or `unverified`.

d-i3vtfymr = Certificate
    .doc = The certificate's own state, as the engine numbers it: 0 when it reported none.

d-zarq7l6o = Issuer

d-5nsqo5b5 = Subject

d-gsqr2kkr = Permissions
    .doc = Each thing the site may ask for, as `camera: ask`, `microphone: blocked`.

d-eygtkatk = Can reset
    .doc = Whether the site may ask again for what it was refused.

d-frfmgq2w = History entry
    .doc = One page the page's own history holds: where it is, what it was called, and the engine's number for it, which going back or forward to it names.
    .noun = { $count ->
        [one] history entry
       *[other] history entries
    }

d-eyk6uw4k = Entry

d-2chmpqlt = Url

d-uugq5btr = Title

d-e43kzg7c = Ask
    .doc = What a page is asked, as the window that holds it reads it: one operation and the fields it takes.
    .noun = { $count ->
        [one] ask
       *[other] asks
    }

d-fqvhxdyr = Op

d-hfpnr6ua = Element

d-tpz54oe5 = Text

d-5vdpb6hg = From

d-wf32vfhg = By

d-q76mm3tr = Drag from

d-byeflqne = Drag steps

d-hxjdnlwy = Frame

d-4udfayix = Frames

d-txp5lrru = X

d-bqydqtva = Y

d-wbhz4hh6 = From x

d-iuschp5g = From y

d-6i5bwl43 = To x

d-hrquaspl = To y

d-5ka7kek5 = Role

d-davmrr4h = Name

d-dmxbh4fm = Nth

d-ao4elp2a = Key

d-jmhhkxjz = Url

d-lqvkggvx = Level

d-zroiv7hs = Values

d-ntpmcicu = Answers
    .doc = What each question of a form is answered with, by the question's name.

d-zoqmh2dz = Muted
    .doc = Whether the page's sound is muted.

d-l3gmhwrx = Direction
    .doc = Which way the page's own history is read: `back` or `forward`.

d-pucrebly = Entry
    .doc = The page's number for one entry of its history, which going there names.

d-4vqdzqut = Doing
    .doc = What a site action does: `clear`, `revoke`, `reset`, `enable`, `disable` or `reload`.

d-fbtgqfhx = Origins
    .doc = The sites tracker protection is off for, which the engine keeps beside its own lists.

d-ymm5jh2h = Origin
    .doc = The site a site action or a status reading is about.

d-7gcvts2u = Generation
    .doc = The document generation the site was read at, which a site action names.

d-ky26jv4a = Paths

d-iabkjt2r = Steps

d-zh65uqqk = Since

d-ersdn54t = Quiet ms

d-bgflyuoe = Timeout ms

d-hjfilxdg = Gone

d-btx3fn7s = Cross origin

d-zf6y6r3k = Secret

d-arhpr4t7 = Submit

d-h6fxerpb = Form

d-cnvdpt2t = Submit button

d-asha7lz3 = To

d-jcrv3nzs = Into view

d-qit2hijp = Button

d-2zpkv6jg = Enabled

d-fggdqfhy = Command

d-sizwtymn = Max width

d-htce2xyp = Id

d-wbwndzhw = Asker

d-nqkenrjw = Username

d-ns2mfzwr = Password

d-qvvu7twk = Typed
    .doc = The answer carries what the person typed answering the ask, filled in on its way to the window from their approve: it is never a value of the run.

d-vwi6rjsr = Accept

d-szlyr265 = Allow

d-acxwp3qh = Forward

d-m6sqrzav = Next

d-e43kzg7c-filter-from = From

d-e43kzg7c-filter-by = By

d-e43kzg7c-filter-drag_steps = Drag steps

d-e43kzg7c-filter-frames = Frames

d-e43kzg7c-filter-x = X

d-e43kzg7c-filter-y = Y

d-e43kzg7c-filter-from_x = From x

d-e43kzg7c-filter-from_y = From y

d-e43kzg7c-filter-to_x = To x

d-e43kzg7c-filter-to_y = To y

d-e43kzg7c-filter-nth = Nth

d-e43kzg7c-filter-level = Level

d-e43kzg7c-filter-muted = Muted

d-e43kzg7c-filter-entry = Entry

d-e43kzg7c-filter-generation = Generation

d-e43kzg7c-filter-since = Since

d-e43kzg7c-filter-quiet_ms = Quiet ms

d-e43kzg7c-filter-timeout_ms = Timeout ms

d-e43kzg7c-filter-cross_origin = Cross origin

d-e43kzg7c-filter-secret = Secret

d-e43kzg7c-filter-submit = Submit

d-e43kzg7c-filter-enabled = Enabled

d-e43kzg7c-filter-command = Command

d-e43kzg7c-filter-max_width = Max width

d-e43kzg7c-filter-id = Id

d-e43kzg7c-filter-typed = Typed

d-e43kzg7c-filter-accept = Accept

d-e43kzg7c-filter-allow = Allow

d-e43kzg7c-filter-forward = Forward

d-e43kzg7c-filter-next = Next

d-nalfirw6 = Answer
    .doc = What a page answered: whether it did what was asked, what it said, and what it found.
    .noun = { $count ->
        [one] answer
       *[other] answers
    }

d-2ubdeo5l = Ok

d-pv4ylrjw = Code
    .doc = Why it did not: the engine's own code, and what it said.

d-d7izzogo = Message

d-ta6htvhh = Url
    .doc = Where the page is, what it is called, whether it is loading and whether it went somewhere.

d-xzex263o = Title

d-htwzro7c = Navigation

d-vptvuat4 = Loading

d-vvykamug = Outline
    .doc = What the page says, line by line: a read's outline, a table's rows, a field's value.

d-i2okiki6 = Element
    .doc = The element a find names, and the outline line that says the words.

d-mmtsrl7r = Line

d-p6uplgxr = Role

d-7zlc443q = Name

d-2ibb6fxi = Frame

d-cxsstht7 = Image
    .doc = A picture of the page, as the PNG the engine painted, base64.

d-hh26i3w2 = Browser
    .doc = The workspace's browser: what the person set, the sites they saved and visited, the forms their pages asked, and the addresses they fill forms with.
    .noun = { $count ->
        [one] browser
       *[other] browsers
    }

d-v73p6lyt = Adblock
    .doc = Tracker and advertisement blocking is on for every site but the ones named below.

d-4mcclqi3 = Clear on exit
    .doc = Cookies, site storage and remembered forms are cleared when the last window closes.

d-zhpmqsvs = Search engine
    .doc = The search engine the address bar asks when what is typed is not an address: the address whose `%s` the words replace.

d-ezeliuhm = Homepage
    .doc = The page a window opens at before anything else.

d-54pfa4xp = Trouble
    .doc = Why the last import could not be taken in, in the file's own words: empty while there is nothing to say.

d-b7avacvg = Sites without protection
    .doc = The sites tracker protection is off for, by exact origin.

d-5kcrev3j = Bookmarks

d-qfwsjkyv = Visits

d-dlo6rb3l = Imported visits

d-m7eeditk = Forms

d-2zm7y6e5 = Addresses

d-zx4a3bhv = Pages

d-z3z5vks2 = Addresses enabled
    .doc = Forms are filled with a saved address while the person has one and leaves this on.

d-hh26i3w2-filter-adblock = Adblock

d-hh26i3w2-filter-clear_on_exit = Clear on exit

d-hh26i3w2-filter-sites_without_protection = Sites without protection

d-hh26i3w2-filter-addresses_enabled = Addresses enabled

d-l4fklfdf = New page
    .doc = Opens a page in a window of its own.

d-2ihrsavk = Start
    .doc = The browser opens on a page: a workspace with none gets its home page as one.

d-ybc7uykj = Open address
    .doc = Opens an address in a page of its own, or asks the search engine for what was typed when it is not one.
    .w-type-an-address-or-what-to = Type an address or what to search for

d-ybc7uykj-typed = Typed

d-eryy3sog = Read the page
    .doc = What the page of `p` says and does, carried to the window that holds it.

d-eryy3sog-from = From

d-eryy3sog-frame = Frame

d-ahqulvrl = Read what changed
    .doc = What changed on the page since the last read: the lines added and removed, or the whole outline when the document is new.

d-ahqulvrl-frame = Frame

d-pqjr74hw = Element at a point
    .doc = What a click at a point would hit, through overlays and banners, and the element holding it.

d-pqjr74hw-x = X

d-pqjr74hw-y = Y

d-pqjr74hw-frame = Frame

d-43i6py3k = Wait for the page
    .doc = Wait for the page to go quiet, or until the words it must say are there and the ones it must not say are gone.

d-43i6py3k-quiet_ms = Quiet ms

d-43i6py3k-timeout_ms = Timeout ms

d-43i6py3k-text = Text

d-43i6py3k-gone = Gone

d-43i6py3k-frame = Frame

d-m4mydrip = Read page console
    .doc = The page's console messages and uncaught errors after a sequence number.

d-m4mydrip-since = Since

d-j2jhcwgf = Read page network
    .doc = The page's network requests after a sequence number: method, address, type, status and failure, never headers, bodies or cookies.

d-j2jhcwgf-since = Since

d-g723ncyu = Read page record
    .doc = The record of who did what in the page, after the place `since` names.

d-g723ncyu-since = Since

d-bv6svg6f = Read page fields
    .doc = Every field of the document with what it holds and who set it.

d-bv6svg6f-frame = Frame

d-cep6j5on = Read page forms
    .doc = The document's forms as what they ask: their questions, the answers they offer, which are required, and the buttons that send them.

d-cep6j5on-frame = Frame

d-igfrldzj = Read page table
    .doc = The page's tables as rows, a long one read in parts.

d-igfrldzj-element = Element

d-igfrldzj-from = From

d-igfrldzj-frame = Frame

d-lfdelqpf = Find page text
    .doc = The outline lines that hold `text`, each with its line number.

d-lfdelqpf-text = Text

d-lfdelqpf-frame = Frame

d-lfdelqpf-frames = Frames

d-5qgw32u3 = Find page element
    .doc = The element the role and name name, and the ref that acts there.

d-5qgw32u3-role = Role

d-5qgw32u3-name = Name

d-5qgw32u3-nth = Nth

d-5qgw32u3-frame = Frame

d-5qgw32u3-frames = Frames

d-jbqrqf3y = Click page
    .doc = Press the element, as a hand does.

d-jbqrqf3y-element = Element

d-jbqrqf3y-frame = Frame

d-d3vp3i7b = Hover page
    .doc = Move the pointer over an element, or to a point of the page.

d-d3vp3i7b-element = Element

d-d3vp3i7b-x = X

d-d3vp3i7b-y = Y

d-u6q76ca7 = Drag page
    .doc = Drag from one element or point to another, in steps.

d-u6q76ca7-from = From

d-u6q76ca7-from_x = From x

d-u6q76ca7-from_y = From y

d-u6q76ca7-to = To

d-u6q76ca7-to_x = To x

d-u6q76ca7-to_y = To y

d-u6q76ca7-steps = Steps

d-v33dekps = Fill page
    .doc = Write `text` into an editable element.

d-v33dekps-element = Element

d-v33dekps-text = Text

d-v33dekps-frame = Frame

d-e73dpir4 = Fill a secret
    .doc = Write a secret into a password or card field: it is never kept in the page's record.

d-e73dpir4-element = Element

d-e73dpir4-text = Text

d-e73dpir4-frame = Frame

d-mniscmpx = Select page
    .doc = Choose options of a select.

d-mniscmpx-element = Element

d-mniscmpx-values = Values

d-mniscmpx-frame = Frame

d-rb2fsv4n = Fill page files
    .doc = Choose the files of a file input, by absolute path.

d-rb2fsv4n-element = Element

d-rb2fsv4n-paths = Paths

d-rb2fsv4n-frame = Frame

d-ls3i6oxj = Press page
    .doc = Press a key in the page, as `Control+a` or `Enter`.

d-ls3i6oxj-key = Key

d-ls3i6oxj-element = Element

d-ls3i6oxj-frame = Frame

d-arvpanvu = Submit page
    .doc = Send the element's form through the page's own validation.

d-arvpanvu-element = Element

d-arvpanvu-cross_origin = Cross origin

d-arvpanvu-frame = Frame

d-3xz2iwbw = Update download
    .doc = Writes what the page last said of one of its downloads into the page's record, as its window hands it over while the transfer moves.

d-3xz2iwbw-id = Id

d-3xz2iwbw-state = State

d-3xz2iwbw-name = Name

d-3xz2iwbw-url = Url

d-3xz2iwbw-path = Path

d-3xz2iwbw-retryable = Retryable

d-3xz2iwbw-received = Received

d-3xz2iwbw-total = Total

d-3xz2iwbw-error = Error

d-ptwqhcz2 = Open from page
    .doc = Keeps a page the page opened in a window of its own as a page of this browser, as its window hands it over: a popup a site sized, or a plain new tab.

d-ptwqhcz2-url = Url

d-ptwqhcz2-popup = Popup

d-7roz5txv = Remember history
    .doc = Writes the pages the page can go to one way into its record, as its window hands them over: what the menu then offers beside Back and Forward.

d-7roz5txv-direction = Direction

d-7roz5txv-entries = Entries

d-4unqkzgu = Answer a form
    .doc = Answers a whole form of the page in one ask: each question's answer by its name, through the page's own validation, and the form's first button when `submit` is on.

d-4unqkzgu-form = Form

d-4unqkzgu-answers = Answers

d-4unqkzgu-submit = Submit

d-4unqkzgu-cross_origin = Cross origin

d-4unqkzgu-frame = Frame

d-ag3d3seg = Pointer page
    .doc = A trusted pointer gesture: press at the first point, move through the rest and release at the last, for drawing on canvases.

d-ag3d3seg-steps = Steps

d-ag3d3seg-button = Button

d-xtqaaha5 = Scroll page
    .doc = Turn the page's wheel to the top or the bottom, into view, or by a distance.

d-xtqaaha5-element = Element

d-xtqaaha5-to = To

d-xtqaaha5-by = By

d-xtqaaha5-into_view = Into view

d-xtqaaha5-frame = Frame

d-g7tvdtci = Capture the page
    .doc = A picture of the page, or of one element's box, as a PNG.

d-g7tvdtci-element = Element

d-g7tvdtci-max_width = Max width

d-g7tvdtci-frame = Frame

d-ckclpobv = Go to an address
    .doc = Open an address in the page's own window.

d-ckclpobv-url = Url

d-gencz4pa = Back
    .doc = Go back to the page before this one.

d-nrcwafmk = Forward
    .doc = Go forward to the page this one followed.

d-fb4z5f2e = Reload
    .doc = Load the page again.

d-f4lsizjk = Stop
    .doc = Stop loading the page.

d-daojkpqh = Reader view
    .doc = Show the page's article without its clutter.

d-daojkpqh-enabled = Enabled

d-d3sudkbf = Zoom
    .doc = Show the page at `level` zoom, where 1 is actual size.

d-d3sudkbf-level = Level

d-i6rhfim2 = Find in page
    .doc = Find words in the page, as the find bar does.

d-i6rhfim2-text = Text

d-i6rhfim2-forward = Forward

d-i6rhfim2-next = Next

d-67gj6gbh = Stop finding
    .doc = Close the find bar's search.

d-hpktlikg = Answer page dialog
    .doc = Answer a page's own question: an alert, a confirmation or a prompt.

d-hpktlikg-id = Id

d-hpktlikg-accept = Accept

d-hpktlikg-text = Text

d-4djvsjzf = Check the page’s questions
    .doc = Reads what the page waits on into its window's record: the question it asks, the menu it showed, and how far it has loaded.

d-u7fbemnk = Opened page
    .doc = The page's window opened: it reads what the page asks and shows before anyone looks.

d-lvbnkhyd = Allow the site
    .doc = Lets a site use what it asked for (its camera, microphone, location, notifications).

d-lvbnkhyd-id = Id

d-lvbnkhyd-origin = Origin

d-lvbnkhyd-wants = Wants

d-p7w6i37f = Save the download
    .doc = Saves the file a page offers.

d-p7w6i37f-id = Id

d-p7w6i37f-name = Name

d-p7w6i37f-url = Url

d-jzj2zsuv = Do not save the download
    .doc = Turns down the file a page offers: nothing is saved, and the page is told so.

d-jzj2zsuv-id = Id

d-jzj2zsuv-name = Name

d-6a4m4f4s = Download an address
    .doc = Saves what an address serves into the browser's downloads.
    .w-type-the-address-to-download = Type the address to download

d-6a4m4f4s-url = Url

d-boafhtzg = Cancel the download
    .doc = Stops a download that is still going.

d-boafhtzg-d = D

d-dmiputmu = Retry the download
    .doc = Starts a failed download again from the beginning.

d-dmiputmu-d = D

d-boev3sbj = Cancel every download
    .doc = Stops every download this page has going.

d-rllv4b5a = Developer tools
    .doc = Opens the engine's developer tools for this page: its console, its elements and its network.

d-k3vqdvsp = Open the download
    .doc = Opens a saved download in the app that opens its kind, through the system's one open.
    .w-save-the-download-first = Save the download first

d-k3vqdvsp-name = Name

d-k3vqdvsp-path = Path

d-hd7ra2gz = Mute the page
    .doc = Silences the page's sound, or lets it play again.

d-hd7ra2gz-muted = Muted

d-ugidwhtg = Earlier pages
    .doc = Asks the page where it can go back to.

d-kwhpfr2t = Later pages
    .doc = Asks the page where it can go forward to, the answer kept like [`read_back_history`]'s.

d-unfwzbsr = Go to a page of its history
    .doc = Goes to one page of the page's own history, by the entry its reading named.

d-unfwzbsr-entry = Entry

d-qdr46mze = Site information
    .doc = Reads the site's connection, its certificate, the requests tracker protection blocked and what the site may ask for, into the page's record.
    .w-this-page-is-not-on-a = This page is not on a site

d-xsnvn6le = Clear this site's data
    .doc = Clears this site's storage, cookies and remembered forms, including cookies it shares with its subdomains, and reloads it.
    .w-read-the-site-s-information-first = Read the site's information first

d-muzeyo2h = Block what this site asks for
    .doc = Blocks what the site may ask for (its camera, microphone, location and notifications).
    .w-read-the-site-s-information-first = Read the site's information first

d-63b5vewx = Let the site ask again
    .doc = Lets the site ask again for what it was refused.
    .w-read-the-site-s-information-first = Read the site's information first

d-kqjsvdfh = Apply the sites tracker protection is off for
    .doc = Tells the engine which sites tracker protection is off for, as the Browser's own set says: the engine keeps the same list beside its filters.

d-trhtau5i = Allow the extension
    .doc = Gives an extension the more access it asked for.

d-trhtau5i-id = Id

d-trhtau5i-extension = Extension

d-trhtau5i-could = Could

d-4xa6zyzf = Sign in to the site
    .doc = Signs in to a site that asks for a user name and a password.
    .take-1 = User name
    .take-2 = Password

d-4xa6zyzf-id = Id

d-4xa6zyzf-asker = Asker

d-4xa6zyzf-site = Site

d-4xa6zyzf-realm = Realm

d-4i65tg5t = Cancel the sign-in
    .doc = Turns down a site's sign-in: nothing is sent, and the server shows its own page instead.

d-4i65tg5t-id = Id

d-4i65tg5t-asker = Asker

d-psytxnym = Let it leave
    .doc = Lets what the page sends leave it: a form, a navigation or a request kept until the person says so.

d-psytxnym-id = Id

d-psytxnym-origin = Origin

d-psytxnym-method = Method

d-psytxnym-leaving = Leaving

d-qbox6son = Context menu action
    .doc = Takes a row of the page's own menu, by the command the engine gave it.

d-qbox6son-id = Id

d-qbox6son-command = Command

d-nwpvyebd = Bookmark a page
    .doc = Saves a page for later.
    .w-a-bookmark-is-an-absolute-http = A bookmark is an absolute http(s) address
    .w-that-page-is-bookmarked-already = That page is bookmarked already

d-nwpvyebd-url = Url

d-nwpvyebd-title = Title

d-4v7rayfx = Remove bookmark
    .doc = Takes a bookmark back.

d-7gjwwcba = Name this bookmark
    .doc = Renames a saved page, when the person likes their own name for it better than its title.
    .w-a-bookmark-is-called-by-some = A bookmark is called by some name

d-7gjwwcba-title = Title

d-vyepjajq = Put the bookmark in a folder
    .doc = Puts a bookmark in a folder of the person's library, such as `Work/Reading`, or at the top of the bookmarks when the folder is empty.

d-vyepjajq-folder = Folder

d-ogasu5l3 = Tracker protection for a site
    .doc = Turns tracker protection off for one site, or back on.
    .w-a-site-is-named-by-one = A site is named by one http(s) origin

d-ogasu5l3-origin = Origin

d-ogasu5l3-enabled = Enabled

d-mnlzylp6 = Protect this site
    .doc = Turns tracker protection off for this site, or back on, as the page's shield does.

d-mnlzylp6-origin = Origin

d-w6wanhhk = Forget this form
    .doc = Forgets one form a site asked.

d-2euwjvpy = Forget this site’s forms
    .doc = Forgets every form a site asked.
    .w-a-site-is-named-by-one = A site is named by one http(s) origin

d-2euwjvpy-origin = Origin

d-wnoovbkq = Forget all remembered forms
    .doc = Forgets every form every site asked.

d-nvkil5qo = Remember form
    .doc = Remembers what a page's forms ask, as its window reads them: the shape of each form and of its questions, never what a field held.
    .w-a-form-is-remembered-of-one = A form is remembered of one http(s) origin
    .w-a-form-is-named-by-its = A form is named by its shape

d-nvkil5qo-origin = Origin

d-nvkil5qo-signature = Signature

d-nvkil5qo-label = Label

d-nvkil5qo-page = Page

d-nvkil5qo-questions = Questions

d-nvkil5qo-whole = Whole

d-xhjamynn = Add an address
    .doc = Adds an address for the forms the person fills.

d-wcffcb7a = Delete address
    .doc = Deletes an address, and stops filling forms with it.

d-eb3wtiud = Fill the form with an address
    .doc = Fills a form of the page with a saved address, question by question, without sending it.
    .w-this-form-asks-nothing-an-address = This form asks nothing an address answers

d-eb3wtiud-form = Form

d-eb3wtiud-a = A

d-luyziriq = Name this visit
    .doc = Names a visit, when the person likes their own name for a page better than its title.
    .w-a-visit-is-called-by-some = A visit is called by some name

d-luyziriq-title = Title

d-zzfcoljy = Forget this page
    .doc = Forgets one visit.

d-tfldgzbg = Clear browsing history
    .doc = Clears the browsing history.

d-2dmo7lzf = Visit
    .doc = Adds a page a window opened to the person's history, counting it when it is there already.

d-2dmo7lzf-url = Url

d-2dmo7lzf-title = Title

d-2dmo7lzf-at = At

d-wrsl2noi = Trim history
    .doc = Keeps the person's own visits to their newest `most`, forgetting the oldest: the history stays within what a person could read, as every other list the system keeps does.
    .w-a-history-keeps-at-least-one = A history keeps at least one page

d-wrsl2noi-most = Most

d-q5brlj4t = Arrived at
    .doc = Takes in a page a window arrived at: it joins the person's history.

d-q5brlj4t-url = Url

d-s7qhc2c6 = Went to
    .doc = The page went somewhere: where it is now is written on the page, and the visit joins the person's history unless the page's window paused it.

d-s7qhc2c6-url = Url

d-32ky3z5c = Pause history
    .doc = Pauses or resumes the person's history in a page's window.

d-32ky3z5c-paused = Paused

d-xuyixcsy = Import browsing data
    .doc = Takes in the browsing data another browser holds, from the bundle this app's own import and `tilo migrate` hand over: its history, its bookmarks, or both.

d-xuyixcsy-file = File

d-ix3ng2as = Import browsing history
    .doc = Imports the pages another browser's history holds, from the bundle this app's own import and `tilo migrate` hand over: a JSON text of `{"{"}"browser": "...", "visits": [{"{"}"url": "...", "title": "...", "last": 1760000000, "count": 1{"}"}]{"}"}`.

d-ix3ng2as-bundle = Bundle

d-y3y3upgg = Import bookmarks
    .doc = Imports the bookmarks another browser holds, from the bundle this app's own import hands over: the `ImportBundle` JSON with bookmark chunks (`{"{"}"bookmarks": [{"{"}"url": "...", "title": "...", "folder": "Work"{"}"}]{"}"}`).

d-y3y3upgg-bundle = Bundle

d-og3g2nds = Ask the page
    .doc = The page `p` does what `request` (JSON, as the window's page reads it) says, and answers the page's own report.

d-og3g2nds-request = Request

d-t3d7dnlx = Go~present

d-tqd4wzqk = Download~present

d-wutb5pms = Open download~present

d-nja545os = Read site~present

d-3wo5bbtr = Clear site data~present

d-h3g4heky = Block site permissions~present

d-j4t6lwtn = Ask site permissions again~present

d-sayctunu = Bookmark~present

d-asdqm6oc = Name bookmark~present

d-qrtmsrs3 = Set site protection~present

d-4j4jisai = Forget site forms~present

d-t5atc53e = Remember form~present

d-xbrdtvx6 = Fill address~present

d-y7tjvycu = Name visit~present

d-sb77wivf = Trim history~present

d-r7exe3by = Page window
    .w-browser = Browser
    .w-no-page-is-open = No page is open
    .w-open-a-page-to-browse-its = Open a page to browse: its address, or what to search for.

d-vsaswu4a = Page view
    .w-back = Back
    .w-forward = Forward
    .w-reload = Reload
    .w-find-in-page = Find in page
    .w-pause-history = Pause history
    .w-address-and-search = Address and search
    .w-bookmark = Bookmark
    .w-questions = Questions
    .w-page = Page
    .w-history = History
    .w-bookmarks = Bookmarks
    .w-browser-settings = Browser settings
    .w-reader-view = Reader view
    .w-unmute-the-page = Unmute the page
    .w-mute-the-page = Mute the page
    .w-earlier-pages = Earlier pages
    .w-later-pages = Later pages
    .w-zoom-in = Zoom in
    .w-zoom-out = Zoom out
    .w-actual-size = Actual size
    .w-actual-size-2 = Actual size ({ $percent }%)
    .w-site-information = Site information
    .w-developer-tools = Developer tools
    .w-turn-tracker-protection-on-for-this = Turn tracker protection on for this site
    .w-turn-tracker-protection-off-for-this = Turn tracker protection off for this site
    .w-forget-this-site-s-forms = Forget this site’s forms
    .w-from-your-history = From your history
    .w-of = { $find_at } of { $find_count }
    .w-previous = Previous
    .w-next = Next
    .w-close = Close
    .w-this-page-could-not-be-shown = This page could not be shown
    .w-this-page-asks = This page asks
    .w-ok = OK
    .w-cancel = Cancel
    .w-requests-blocked = Requests blocked
    .w-certificate = Certificate
    .w-clear-this-site-s-data = Clear this site's data
    .w-block-what-this-site-asks-for = Block what this site asks for
    .w-let-the-site-ask-again = Let the site ask again
    .w-downloads = Downloads
    .w-cancel-every-download = Cancel every download
    .w-what-was-done = What was done

d-23a4az4u = Suggestion row

d-pu2zdrbu = Step row

d-uppzh7ic = Bookmark page
    .w-address = Address
    .w-details = Details
    .w-name = Name
    .w-folder = Folder
    .w-top-of-the-bookmarks = Top of the bookmarks
    .w-remove-bookmark = Remove bookmark

d-fxq6nhpy = Address page
    .w-the-address = The address
    .w-name-for-it = Name for it
    .w-home = Home
    .w-full-name = Full name
    .w-given-name = Given name
    .w-family-name = Family name
    .w-street = Street
    .w-city = City
    .w-region = Region
    .w-postal-code = Postal code
    .w-country = Country
    .w-delete-address = Delete address

d-y2shuesh = Visit page
    .w-address = Address
    .w-last-opened = Last opened
    .w-opened = Opened
    .w-forget-this-page = Forget this page

d-jlg4rve3 = Library
    .w-bookmarks = Bookmarks
    .w-history = History

d-3dld2nsk = Bookmarks list
    .w-no-bookmarks-yet = No bookmarks yet
    .w-top-of-the-bookmarks = Top of the bookmarks

d-2ow3mpb2 = Visit row

d-okndzb5z = Imported row

d-qgyn4224 = Bookmark row

d-b7sixoyk = Address row

d-sij6peeq = Form row
    .w-read-times = { $origin } · read { $seen } times
    .w-required = { $label } ({ $answer }, required)
    .w-forget-this-form = Forget this form

d-ok2yuain = Browser settings
    .w-browser-settings = Browser settings
    .w-something-could-not-be-taken-in = Something could not be taken in
    .w-browsing = Browsing
    .w-block-advertisements-and-trackers = Block advertisements and trackers
    .w-clear-cookies-site-storage-and-remembered = Clear cookies, site storage and remembered forms when the last window closes
    .w-search-engine = Search engine
    .w-https-duckduckgo-com-q-s = https://duckduckgo.com/?q=%s
    .w-home-page = Home page
    .w-about-blank = about:blank
    .w-addresses = Addresses
    .w-fill-forms-with-a-saved-address = Fill forms with a saved address
    .w-add-an-address = Add an address
    .w-sites-without-tracker-protection = Sites without tracker protection
    .w-every-site-is-protected = Every site is protected.
    .w-protect-this-site-again = Protect this site again
    .w-remembered-forms = Remembered forms
    .w-no-site-s-forms-were-read = No site’s forms were read yet.
    .w-forget-all-remembered-forms = Forget all remembered forms

d-36vwukxk = History list
    .w-no-pages-visited-yet = No pages visited yet
    .w-from-another-browser = From another browser

d-6w4zu5de = Download row
    .w-downloading = Downloading
    .w-cancel = Cancel
    .w-retry = Retry
    .w-open = Open
