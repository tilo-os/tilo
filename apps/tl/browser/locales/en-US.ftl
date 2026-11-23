# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-browser-preview = Browser

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

d-ybc7uykj = Open address
    .doc = Opens an address in a page of its own, or asks the search engine for what was typed when it is not one.
    .w-type-an-address-or-what-to = Type an address or what to search for

d-ybc7uykj-typed = Typed

d-nwpvyebd = Bookmark a page
    .doc = Saves a page for later.
    .w-a-bookmark-is-an-absolute-http = A bookmark is an absolute http(s) address
    .w-that-page-is-bookmarked-already = That page is bookmarked already

d-nwpvyebd-url = Url

d-nwpvyebd-title = Title

d-4v7rayfx = Remove bookmark
    .doc = Takes a bookmark back.

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

d-q5brlj4t = Arrived at
    .doc = Takes in a page a window arrived at: it joins the person's history.

d-q5brlj4t-url = Url

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

d-t3d7dnlx = Go~present

d-sayctunu = Bookmark~present

d-qrtmsrs3 = Set site protection~present

d-4j4jisai = Forget site forms~present

d-t5atc53e = Remember form~present

d-y7tjvycu = Name visit~present

d-r7exe3by = Page window
    .w-browser = Browser
    .w-address-and-search = Address and search
    .w-bookmark = Bookmark
    .w-this-page-could-not-be-shown = This page could not be shown

d-o5lcfrvr = Page menu
    .w-page = Page
    .w-history = History
    .w-bookmarks = Bookmarks
    .w-browser-settings = Browser settings
    .w-turn-tracker-protection-on-for-this = Turn tracker protection on for this site
    .w-turn-tracker-protection-off-for-this = Turn tracker protection off for this site
    .w-forget-this-site-s-forms = Forget this site’s forms

d-vsaswu4a = Page view
    .w-pause-history = Pause history
    .w-bookmark = Bookmark
    .w-this-page-could-not-be-shown = This page could not be shown
    .w-what-was-done = What was done

d-pu2zdrbu = Step row

d-uppzh7ic = Bookmark page
    .w-address = Address
    .w-folder = Folder
    .w-bookmarks = Bookmarks
    .w-remove-bookmark = Remove bookmark

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

d-2ow3mpb2 = Visit row

d-okndzb5z = Imported row

d-qgyn4224 = Bookmark row

d-b7sixoyk = Address row

d-sij6peeq = Form row
    .w-read-times = { $origin } · read { $seen } times
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
    .w-remembered-forms = Remembered forms
    .w-no-site-s-forms-were-read = No site’s forms were read yet.
    .w-forget-all-remembered-forms = Forget all remembered forms

d-36vwukxk = History list
    .w-no-pages-visited-yet = No pages visited yet
    .w-from-another-browser = From another browser
