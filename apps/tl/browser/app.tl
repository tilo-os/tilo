app so.tilo.browser "Browser" (
    icon: .globe,
    category: .network,
    tagline: "Web browser with ad blocking and Chrome extensions",
    engine: "tilo-runtime/2026.10.23-chromium",
    keywords: ["web", "internet", "surf", "website", "url", "www", "web browser", "chrome", "safari", "edge", "firefox", "browser", "browsen", "webseite", "internetseite"],
    schemes: ["http", "https", "web+search"],
)

use so.tilo.file-explorer as files
use so.tilo.print-centre as printing

op "download"

/// A site someone saved: where it is and what it is called.
type Bookmark (title: title) {
    url: Text
    title: Text
    /// The folder the bookmark stands in, as a path in the person's own library (`Work/Reading`):
    /// empty for one at the top. Bookmarks imported from another browser keep their folders.
    folder: Text = ""

    search {
        sort title a_z
        hide folder
    }
}

/// A page the person visited: where it is, what it was called when they last read it, and when
/// that was. Visiting it again counts up rather than making a second visit.
type Visit (title: title) {
    url: Text
    title: Text
    /// When the person last opened it, on their clock.
    last: Time
    /// How many times a window opened it.
    count: Int merge counter

    search {
        sort last newest
        hide url
    }
}

/// A page taken in from another browser's history, kept beside the visits this browser made and
/// counted apart, so importing the same history twice keeps one page.
type ImportedVisit (title: title) {
    url: Text
    title: Text
    last: Time
    count: Int merge counter

    search {
        sort last newest
    }
}

/// Where a row the address bar offers comes from.
enum SuggestionKind {
    /// A page the person saved.
    bookmark
    /// A page this browser opened.
    visited
    /// A page another browser's history brought over.
    imported
    /// The search engine, asked for what was typed.
    search
}

/// One row the address bar offers for what was typed: a page the person saved or opened, or the
/// search for the words.
type Suggestion {
    kind: SuggestionKind
    title: Text
    /// What choosing it opens: the page, or the search engine's answer to the words.
    url: Text
    /// What the row says under its title: the page's address, or the engine that is asked.
    secondary: Text
}

/// A site's remembered form: what it asked and what it called each question, never an answer.
type RememberedForm (title: label) {
    /// The site's origin (`https://example.org`), as tracker protection knows it too.
    origin: Text
    /// The form's shape, which names the same form on a later visit and stops naming it once the
    /// form asks something else.
    signature: Text
    /// What the form is called: its own legend, or the page it stands on.
    label: Text
    /// The pages of the site the form was read on.
    pages: List<Text>
    /// How many times the form was read.
    seen: Int merge counter
    /// When it was last read, on the person's clock.
    read_at: Time = Time("1970-01-01T00:00:00Z")
    questions: Collection<Question>

    search {
        sort label a_z
        hide signature
    }
}

/// One question of a remembered form: what it is called, what it asks for and the answers it
/// offers.
type Question {
    name: Text
    /// What kind of answer it takes: `text`, `email`, `tel`, `number`, `url`, `password`,
    /// `choice`, `choices`, `file`, `date` or `checkbox`.
    answer: Text
    /// What the question asks, as the page labels it.
    label: Text
    choices: List<Text>
    required: Bool = false
}

/// An address the person fills forms with, offered where a form asks for one.
type Address (title: label) {
    label: Text
    full_name: Text
    given_name: Text = ""
    family_name: Text = ""
    street: Text = ""
    city: Text = ""
    region: Text = ""
    postal_code: Text = ""
    country: Text = ""
}

/// A page open in a window: its address, what it is called, and what was done in it, newest
/// last. The page itself lives in the engine on this device; what every member of the workspace
/// reads of it is this. A private window's page stays on the device it was opened on, its
/// author's alone, and goes when its window closes or the device stops.
type Page (title: title, readers: author here until shared) {
    url: Text
    title: Text
    /// History is paused in this page's window: pages opened there are not added to the person's
    /// history.
    paused: Bool = false
    /// Every member of the workspace reads the page. A private window's page is not shared: it
    /// stays on this device, never written to disk or sent anywhere, so no other member, agent
    /// or device of the person reads it. Its engine runs on a profile of its own, which holds
    /// nothing the person signed in to, is not offered their saved logins, and goes when the
    /// window closes, as the page does. Nothing it opens joins the person's history.
    shared: Bool = true
    /// The page was opened for an agent, which browses it as any visitor would: in every window
    /// it opens in, its engine runs on a profile of its own, which holds none of the person's
    /// cookies or sign-ins, is not offered their saved logins, and goes when the window closes.
    visitor: Bool = false
    /// The website account of the person's (Slack, Discord) the page opens, on a profile of its
    /// own signed in as that account: empty for a page of the person's own profile.
    account: Text = ""
    /// That the account is on its way, or why it did not open, in the person's words: empty once
    /// it is open.
    account_notice: Text = ""
    /// What was done in the page, newest last. The newest thousand are kept.
    steps: Collection<PageStep>
    /// What the page's own script asks, and the menu it showed, as its window last read them.
    /// Engine state, refreshed when the page's window opens it and after every answer, never the
    /// page's own record. What the page waits on the person's yes for (a site's permission, a
    /// download, a sign-in, a request leaving) is asked as the system's asked record instead.
    dialog: Option<DialogAsk> = .none
    menu: Option<MenuAsk> = .none
    /// Whether the page's sound is muted.
    muted: Bool = false
    /// The video the page shows, as its window last read it: the picture "Picture in picture"
    /// floats above the window. Engine state, never the page's own record.
    media: Option<PageMedia> = .none
    /// Whether the page's video floats above its window as picture-in-picture.
    pip: Bool = false
    /// The page is filling the screen: a video, a slide deck or a page's own request put it
    /// there, and its window shows the page alone with the way out over it.
    fullscreen: Bool = false
    /// The site's connection, permissions and stored data, as its window last read them.
    site: Option<SiteInfo> = .none
    /// The login forms the engine found on this page.
    login_forms: Collection<LoginForm>
    /// The logins the workspace's vault keeps for this page's site, by name: never a password.
    login_accounts: Collection<LoginAccount>
    /// The capture the engine took of a login this page sent, until it is saved or dropped.
    capture: Text = ""
    /// What the last login action came to, in the engine's or the vault's words.
    login_notice: Text = ""
    /// Why the page could not be printed, in the person's words; empty once one is on its way
    /// to Print Centre or was before.
    print_notice: Text = ""
    /// The toolbar buttons this page's extensions show, as the engine last reported them.
    buttons: Collection<PageButton>
    /// The notifications this page's extensions showed and have not taken back.
    notices: Collection<ExtensionNotice>
    /// The extension's page the page's window shows beside this one, as it last said: the
    /// extension's side panel, or the page it asked the person to sign in on.
    panel: Option<Panel> = .none
    /// The extension the address addresses, and what it offers for what follows its keyword.
    omnibox: Option<Omnibox> = .none
    /// The pages before this one and the ones after it, as the engine last answered the window's
    /// request for them.
    back: Collection<HistoryEntry>
    forward: Collection<HistoryEntry>
    /// How far the page has loaded, what it is zoomed to, and where the find bar stands.
    loading: Bool = false
    progress: Float = 0.0
    zoom_level: Float = 1.0
    find_at: Int = 0
    find_count: Int = 0
    /// The words the window's search found last: what the way on to the next and back to the
    /// previous match asks the page for.
    find_text: Text = ""

    search {
        sort title a_z
        hide url
    }
}

/// One thing done in a page: who asked, what was done, what it acted on and how it ended. A
/// secret is never written here, and reading the page takes no place in it.
type PageStep {
    /// When it was done, in milliseconds since the page opened.
    at: Int
    /// Who asked: a person, an agent, a script.
    actor: Option<Actor> = .none
    /// The device the actor worked from: empty for this one.
    device: Text = ""
    device_name: Text = ""
    /// What was done, as the page's actions name it.
    operation: Text
    ok: Bool = true
    code: Text = ""
    message: Text = ""
    /// The address it acted on, and the one it went to, for a step that navigated.
    url: Text = ""
    to_url: Text = ""
    /// The element it acted on, by its role and its name.
    role: Text = ""
    element: Text = ""
    /// The text it wrote, or the choices it made.
    text: Option<Text> = .none
    values: Option<List<Text>> = .none
}

/// A question the page's own script asks: an alert, a confirmation, a prompt or a page leaving.
type DialogAsk (title: message) {
    /// The engine's id for the question, which the answer names.
    request: Int
    /// `alert`, `confirm`, `prompt` or `leave`.
    kind: Text
    origin: Text
    message: Text
    default_text: Text = ""
}

/// The menu the page showed: its rows, and where it showed them.
type MenuAsk (title: link) {
    request: Int
    link: Text = ""
    items: List<MenuRow>
}

/// One row of the page's menu: a command, or a rule between groups.
type MenuRow (title: label) {
    command: Int = 0
    label: Text = ""
    disabled: Bool = false
    checked: Bool = false
    separator: Bool = false
}

/// The video a page shows, as the engine last reported it: what "Picture in picture" floats
/// above the window, where the page draws it. Engine state like the page's own questions,
/// never the page's own record.
type PageMedia (title: label) {
    /// The engine's id for the element.
    element: Text
    label: Text = ""
}

/// One file the Browser downloads, from any of its windows: where it comes from, what it is
/// called, and the device job that fetches it into the person's Downloads. How far it has come
/// and where it went are the job's, read as it moves; the job goes on when the window closes.
type Download (title: name) {
    url: Text
    name: Text
    /// The job fetching it: a retry starts another, which the download keeps instead.
    job: Text = ""
    /// Where a download an extension started stands, as the page's engine, which saves it,
    /// tells: none for one the Browser fetches itself.
    seen: Option<DownloadJob> = .none
    /// The page engine's number for a download an extension started.
    engine: Int = 0

    search {
        sort name a_z
        hide url
    }
}

/// Where a finished download was saved and as what, as its job's record says.
type DownloadSaved {
    path: Text = ""
    name: Text = ""
}

/// Where a download's job stands, as the job tells it: `running`, `done`, `failed` or
/// `cancelled`, how many bytes of how many (0 while the size is unknown), where it was saved and
/// as what, and why it failed.
type DownloadJob {
    state: Text
    done: Int = 0
    total: Int = 0
    path: Text = ""
    name: Text = ""
    error: Text = ""
}

/// What the Browser asks its engine to download: the address, the name the page suggested, and
/// the handle the page's window keeps the request's context under (the site's cookies never
/// travel in a call).
type DownloadAsk {
    url: Text
    name: Text
    request: Text
}

/// What a site's connection, its permissions and its stored data are, as the page's window last
/// read them: engine state, refreshed when the window reads the page.
type SiteInfo (title: origin) {
    origin: Text
    /// The document's generation the engine answered for: a site action names it, so a command
    /// meant for this page never lands on one that came after.
    generation: Int
    /// How many requests in this page tracker protection blocked or replaced.
    blocked: Int
    /// Whether tracker protection is on for this site.
    protected: Bool = true
    /// `secure`, `mixed`, `insecure` or `unverified`.
    security: Text
    /// The certificate's own state, as the engine numbers it: 0 when it reported none.
    certificate: Int = 0
    issuer: Text
    subject: Text
    /// Each thing the site may ask for, as `camera: ask`, `microphone: blocked`.
    permissions: List<Text>
    /// Whether the site may ask again for what it was refused.
    can_reset: Bool = false

    search {
        hide origin
    }
}

/// One page the page's own history holds: where it is, what it was called, and the engine's
/// number for it, which going back or forward to it names.
type HistoryEntry (title: title) {
    entry: Int
    url: Text
    title: Text

    search {
        hide url
    }
}

/// One extension's button in the page's toolbar: what it shows, whether it takes a press, and
/// whether pressing it opens a page of the extension's own.
type PageButton (title: label) {
    /// The engine's identifier of the extension the button stands for.
    ext: Text
    label: Text
    badge: Text = ""
    enabled: Bool = true
    popup: Bool = false

    search {
        hide ext
    }
}

/// One notification an extension showed: the extension it is from, the extension's own number
/// for it, what it says, and the buttons it offers.
type ExtensionNotice (title: title) {
    /// The engine's identifier of the extension that showed it.
    ext: Text
    notification: Text
    title: Text
    message: Text
    buttons: List<Text>

    search {
        hide ext
    }
}

/// A page shown beside the page the person browsed: an extension's side panel, the page it
/// asked them to sign in on, or a popup a script opened.
type Panel (title: ext) {
    /// `side` for an extension's side panel, `sign-in` for the page it asked the person to sign in
    /// on, `popup` for a page a script opened in a window the site sized.
    kind: Text
    /// The engine's identifier of the extension whose page it is, or of the popup.
    ext: Text
    /// The address the page shows.
    url: Text
    /// Whether the person is shown it: a sign-in they may not be asked on loads out of sight.
    interactive: Bool = true
}

/// What the address offers the extension whose keyword it addresses: what follows the keyword,
/// and the rows the extension says choosing leads to.
type Omnibox (title: ext) {
    /// The engine's identifier of the extension the address addresses.
    ext: Text
    /// What that extension is called.
    name: Text
    /// What follows its keyword.
    text: Text
    /// What the address offers: the first row is what Enter does, then the extension's
    /// suggestions.
    rows: List<OmniboxRow>
}

/// One row the address offers for an extension's keyword: what it says, and what choosing it
/// sends the extension.
type OmniboxRow (title: title) {
    title: Text
    content: Text
}

/// An extension a keyword in the address addresses: which one it is, what it is called, and what
/// follows its keyword.
type Addressed {
    ext: Text
    name: Text
    text: Text
}

/// One extension installed on this device: what it is, what its manifest asks for, and whether
/// the person turned it on. Chromium loads the ones that are on when a page's window starts.
type Extension (title: name) {
    /// The engine's identifier for it, which turning it on or removing names.
    key: Text
    name: Text
    version: Text
    manifest_version: Int
    description: Text
    /// What it asks for, as its own words, one after another.
    permissions: Text
    enabled: Bool = false
    /// Its keyword in the address (`omnibox.keyword`), where its manifest gives one: typed with a
    /// space after it, what follows is the extension's.
    keyword: Text = ""
    /// Whether it can show a page beside the page the person browses (`sidePanel`).
    side_panel: Bool = false

    search {
        hide key
    }
}

/// One login form the engine found on the page: its own number for it, the account it offers,
/// and what kind the engine says it is (`current-password` for signing in, `new-password` for
/// setting one).
type LoginForm (title: form) {
    form: Text
    username: Text
    kind: Text

    search {
        hide form
    }
}

/// One login the workspace's vault keeps for this site: the vault item it is (`vault.item:<id>`),
/// what it is called, and the name it signs in as. Its password stays in the vault, goes only to
/// the page's engine when it fills a form, and never reaches the app.
type LoginAccount (title: username) {
    account: Text
    username: Text
    title: Text = ""
    /// Whether the Browser saved it itself: filling it asks nobody. Any other login of the site
    /// is filled once its person allowed the Browser that one login.
    own: Bool = false

    search {
        hide account
    }
}

/// What a page is asked, as the window that holds it reads it: one operation and the fields it
/// takes. A field left at its default is unsent, and the page reads it as absent.
type Ask {
    op: Text
    element: Option<Text> = .none
    text: Option<Text> = .none
    from: Option<Int> = .none
    by: Option<Int> = .none
    drag_from: Option<Text> = .none
    drag_steps: Option<Int> = .none
    frame: Option<Text> = .none
    frames: Bool = false
    x: Option<Float> = .none
    y: Option<Float> = .none
    from_x: Option<Float> = .none
    from_y: Option<Float> = .none
    to_x: Option<Float> = .none
    to_y: Option<Float> = .none
    role: Option<Text> = .none
    name: Option<Text> = .none
    nth: Option<Int> = .none
    key: Option<Text> = .none
    url: Option<Text> = .none
    /// The job a retried download ended as, which the page's answer names again.
    job: Option<Text> = .none
    /// The address of an extension package the person chose, in this device's content store.
    file: Option<Text> = .none
    /// The Chrome Web Store item an install names: its page's address, or the extension's id.
    item: Option<Text> = .none
    /// The identifier of an installed extension, which turning it on or removing names.
    extension: Option<Text> = .none
    /// What follows an extension's keyword in the address, which the extension hears of.
    rest: Option<Text> = .none
    /// The extension's own number for one of its notifications.
    notification: Option<Text> = .none
    level: Option<Float> = .none
    values: Option<List<Text>> = .none
    /// What each question of a form is answered with, by the question's name.
    answers: Option<Map<Text>> = .none
    /// Whether the page's sound is muted.
    muted: Bool = false
    /// Which way the page's own history is read: `back` or `forward`.
    direction: Option<Text> = .none
    /// The page's number for one entry of its history, which going there names.
    entry: Option<Int> = .none
    /// What a site action does: `clear`, `revoke`, `reset`, `enable`, `disable` or `reload`.
    doing: Option<Text> = .none
    /// The sites tracker protection is off for, which the engine keeps beside its own lists.
    origins: Option<List<Text>> = .none
    /// The site a site action or a status reading is about.
    origin: Option<Text> = .none
    /// The login action: the engine's own `status`, `cancel`, `discover`, `addresses`,
    /// `capture` or `fill_address`, or `list`, `save`, `update`, `fill` or `delete`, which the
    /// window does with the workspace's vault.
    credential: Option<Text> = .none
    /// The capture the engine took of a login form, which saving or updating names.
    capture: Option<Text> = .none
    /// The vault item of a saved login, which filling, updating or forgetting names, or the
    /// website account of the person's a page opens, by its id.
    account: Option<Text> = .none
    /// The address a form is filled from, through the engine.
    address: Option<Address> = .none
    /// The document generation the site was read at, which a site action names.
    generation: Option<Int> = .none
    paths: Option<List<Text>> = .none
    steps: Option<List<Text>> = .none
    since: Option<Int> = .none
    quiet_ms: Option<Int> = .none
    timeout_ms: Option<Int> = .none
    gone: Option<Text> = .none
    cross_origin: Bool = false
    secret: Bool = false
    submit: Bool = false
    form: Option<Text> = .none
    submit_button: Option<Text> = .none
    to: Option<Text> = .none
    into_view: Option<Text> = .none
    button: Option<Text> = .none
    enabled: Bool = false
    command: Option<Int> = .none
    max_width: Option<Int> = .none
    id: Option<Int> = .none
    asker: Option<Text> = .none
    username: Option<Text> = .none
    password: Option<Text> = .none
    /// The answer carries what the person typed answering the ask, filled in on its way to the
    /// window from their approve: it is never a value of the run.
    typed: Bool = false
    accept: Bool = false
    allow: Bool = false
    forward: Bool = false
    next: Bool = false
}

/// What a page answered: whether it did what was asked, what it said, and what it found.
type Answer (title: message) {
    ok: Bool = false
    /// Why it did not: the engine's own code, and what it said.
    code: Text = ""
    message: Text = ""
    /// Where the page is, what it is called, whether it is loading and whether it went
    /// somewhere.
    url: Text = ""
    title: Text = ""
    navigation: Text = ""
    loading: Bool = false
    /// What the page says, line by line: a read's outline, a table's rows, a field's value.
    outline: Text = ""
    /// The element a find names, and the outline line that says the words.
    element: Text = ""
    line: Int = 0
    role: Text = ""
    name: Text = ""
    frame: Text = ""
    /// A picture of the page, as the PNG the engine painted, base64.
    image: Text = ""

    search {
        hide url
        hide image
    }
}

/// A website account of the person's (Slack, Discord) that opens in a window of its own, on a
/// profile of its own signed in as that account: one they brought from a browser of theirs or
/// connected here. Theirs alone, as their window last listed it: its cookies stay in the
/// workspace's vault.
type WebsiteAccount (readers: author) {
    /// The account, as the workspace's accounts name it.
    account: Text
    /// The service it signs in to: `slack` or `discord`.
    service: Text
    /// What the person calls it.
    title: Text
}

/// The workspace's browser: what the person set, the sites they saved and visited, the forms
/// their pages asked, and the addresses they fill forms with. One workspace holds one, and each
/// member sees the same one.
type Browser {
    /// Tracker and advertisement blocking is on for every site but the ones named below.
    adblock: Bool = true
    /// Cookies, site storage and remembered forms are cleared when the last window closes.
    clear_on_exit: Bool = false
    /// The search engine the address bar asks when what is typed is not an address: the address
    /// whose `%s` the words replace.
    search_engine: Text = "https://duckduckgo.com/?q=%s"
    /// The page a window opens at before anything else.
    homepage: Text = ""
    /// Why the last import could not be taken in, in the file's own words: empty while there is
    /// nothing to say.
    trouble: Text = ""
    /// The sites tracker protection is off for, by exact origin.
    sites_without_protection: Set<Text>
    bookmarks: Collection<Bookmark>
    visits: Collection<Visit>
    imported_visits: Collection<ImportedVisit>
    forms: Collection<RememberedForm>
    addresses: Collection<Address>
    pages: Collection<Page>
    /// The person's website accounts that open in a window of their own, each theirs alone.
    accounts: Collection<WebsiteAccount>
    /// Forms are filled with a saved address while the person has one and leaves this on.
    addresses_enabled: Bool = false
    /// The extensions installed on this device, as a page's window last listed them.
    extensions: Collection<Extension>
    /// The files downloaded from every window, newest last: each outlives the page it came from.
    downloads: Collection<Download>
    /// What installing, turning on or removing an extension came to, in the person's words:
    /// empty while there is nothing to say.
    extension_notice: Text = ""
}

root Browser

default view page_window for Browser

default view page_view for Page

default view bookmark_page for Bookmark

default view visit_page for Visit

default view address_page for Address

/// Opens a page in a window of its own. Every window of the app is one page; what it shows, its
/// address and its own back and forward are the system's.
primary action new_page(b: Browser) -> Page (icon: .plus, label: "New page", key: "t") {
    b.pages.insert(Page(url: b.homepage, title: ""))
}

/// Opens a window that browses privately: nothing it reads, signs in to or downloads joins what
/// this device keeps of the person.
action new_private_page(b: Browser) -> Page (
    icon: .eye_off,
    label: "New private window",
    key: "Ctrl+Shift+P",
) {
    b.pages.insert(Page(url: b.homepage, title: "", shared: false))
}

/// Opens a website account of the person's (Slack, Discord) in a window of its own, on a profile of
/// its own signed in as that account: none of their other sign-ins reach it, and it reaches none
/// of theirs.
action open_account(b: Browser, a: WebsiteAccount) -> Page (icon: .user, label: "Open account") {
    let title = a.title
    b.pages.insert(
        Page(
            url: "",
            title: title,
            account: a.account,
            account_notice: std.say("Opening {title}…"),
        ),
    )
}

/// A private window closed: its page goes with it, so nothing of it outlives the window.
closed action close_private_page(p: Page) {
    if not p.shared {
        let b = root Browser
        b.pages.remove(p)
    }
}

/// The browser opens on a page: a workspace with none gets its home page as one.
initial action start(b: Browser) {
    if b.pages.is_empty() {
        b.pages.insert(Page(url: b.homepage, title: ""))
    }
}

/// Opens an address in a page of its own, or asks the search engine for what was typed when it
/// is not one.
action go(b: Browser, typed: Text) -> Page (icon: .arrow_right, label: "Open address") {
    require typed.trim() != "" "Type an address or what to search for"
    let url = resolve(b, typed)
    b.pages.insert(Page(url: url, title: ""))
}

/// Opens a link of a scheme this browser claims: the address in a page of its own, or the search
/// a `web+search:` link asks for. A shell hands a link here whether or not a window of the app is
/// open, and what this returns opens in the window.
action open_link(b: Browser, link: Text) -> Page (icon: .external_link, label: "Open link") {
    let asked = link.trim()
    let search = "web+search:"
    if asked.starts_with(search) {
        let words = asked.slice(search.len(), asked.len()).replace("+", " ").replace("%20", " ")
        b.pages.insert(Page(url: resolve(b, words), title: ""))
    } else {
        let url = normalized(asked)
        b.pages.insert(Page(url: if url == "" { b.homepage } else { url }, title: ""))
    }
}

/// What the page of `p` says and does, carried to the window that holds it. Each action answers
/// the page's own report as an [`Answer`], and what was done is written into the page's record.
///
/// Read the page: what it says, line by line, as an agent reads it. A long page is read in
/// parts: a read that stops short names the line to read on from.
action read_page(
    p: Page,
    from: Option<Int> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .eye, label: "Read the page") {
    let request = json.encode(Ask(op: "outline", from: from, frame: frame))
    ask_page(p, request)
}

/// What changed on the page since the last read: the lines added and removed, or the whole
/// outline when the document is new.
action read_page_changes(p: Page, frame: Option<Text> = .none) -> Answer uses page (
    icon: .eye,
    label: "Read what changed",
) {
    let request = json.encode(Ask(op: "changes", frame: frame))
    ask_page(p, request)
}

/// What a click at a point would hit, through overlays and banners, and the element holding it.
action read_page_element_at(
    p: Page,
    x: Float,
    y: Float,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .mouse_pointer_2, label: "Element at a point") {
    let request = json.encode(Ask(op: "at", x: .some(x), y: .some(y), frame: frame))
    ask_page(p, request)
}

/// Wait for the page to go quiet, or until the words it must say are there and the ones it must
/// not say are gone.
action wait_page_settled(
    p: Page,
    quiet_ms: Option<Int> = .none,
    timeout_ms: Option<Int> = .none,
    text: Option<Text> = .none,
    gone: Option<Text> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .clock, label: "Wait for the page") {
    let request = json.encode(
        Ask(
            op: "settle",
            quiet_ms: quiet_ms,
            timeout_ms: timeout_ms,
            text: text,
            gone: gone,
            frame: frame,
        ),
    )
    ask_page(p, request)
}

/// The page's console messages and uncaught errors after a sequence number.
action read_page_console(p: Page, since: Option<Int> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "console", since: since))
    ask_page(p, request)
}

/// The page's network requests after a sequence number: method, address, type, status and
/// failure, never headers, bodies or cookies.
action read_page_network(p: Page, since: Option<Int> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "network", since: since))
    ask_page(p, request)
}

/// The record of who did what in the page, after the place `since` names.
action read_page_record(p: Page, since: Option<Int> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "log", since: since))
    ask_page(p, request)
}

/// Every field of the document with what it holds and who set it. A secret is listed by length.
action read_page_fields(p: Page, frame: Option<Text> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "fields", frame: frame))
    ask_page(p, request)
}

/// The document's forms as what they ask: their questions, the answers they offer, which are
/// required, and the buttons that send them.
action read_page_forms(p: Page, frame: Option<Text> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "forms", frame: frame))
    ask_page(p, request)
}

/// The page's tables as rows, a long one read in parts.
action read_page_table(
    p: Page,
    element: Option<Text> = .none,
    from: Option<Int> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(Ask(op: "table", element: element, from: from, frame: frame))
    ask_page(p, request)
}

/// The outline lines that hold `text`, each with its line number.
action find_page_text(
    p: Page,
    text: Text,
    frame: Option<Text> = .none,
    frames: Bool = false,
) -> Answer uses page {
    let request = json.encode(Ask(op: "search", text: .some(text), frame: frame, frames: frames))
    ask_page(p, request)
}

/// The element the role and name name, and the ref that acts there.
action find_page_element(
    p: Page,
    role: Option<Text> = .none,
    name: Option<Text> = .none,
    nth: Option<Int> = .none,
    frame: Option<Text> = .none,
    frames: Bool = false,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "find", role: role, name: name, nth: nth, frame: frame, frames: frames),
    )
    ask_page(p, request)
}

/// Press the element, as a hand does.
action click_page(p: Page, element: Text, frame: Option<Text> = .none) -> Answer uses page {
    let request = json.encode(Ask(op: "click", element: .some(element), frame: frame))
    ask_page(p, request)
}

/// Move the pointer over an element, or to a point of the page.
action hover_page(
    p: Page,
    element: Option<Text> = .none,
    x: Option<Float> = .none,
    y: Option<Float> = .none,
) -> Answer uses page {
    let request = json.encode(Ask(op: "hover", element: element, x: x, y: y))
    ask_page(p, request)
}

/// Drag from one element or point to another, in steps.
action drag_page(
    p: Page,
    from: Option<Text> = .none,
    from_x: Option<Float> = .none,
    from_y: Option<Float> = .none,
    to: Option<Text> = .none,
    to_x: Option<Float> = .none,
    to_y: Option<Float> = .none,
    steps: Option<Int> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(
            op: "drag",
            drag_from: from,
            from_x: from_x,
            from_y: from_y,
            to: to,
            to_x: to_x,
            to_y: to_y,
            drag_steps: steps,
        ),
    )
    ask_page(p, request)
}

/// Write `text` into an editable element.
action fill_page(
    p: Page,
    element: Text,
    text: Text,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "fill", element: .some(element), text: .some(text), frame: frame),
    )
    ask_page(p, request)
}

/// Fill a password field from a login the workspace's vault keeps, named by its reference
/// (`vault.item:<id>`). The window asks the vault for it for the site its engine says the page
/// is at, and the engine writes it only into that site's own page: a login kept for another
/// site fills nothing. The secret goes from the vault into the field, never through this
/// action, its answer or the page's record; how the fill ended is the page's login notice.
action fill_page_secret(p: Page, element: Text, item: Text) -> Answer uses page (
    icon: .key,
    label: "Fill a secret",
) {
    ask_page(
        p,
        json.encode(
            Ask(
                op: "credential",
                credential: .some("fill_field"),
                origin: .some(origin_of(p.url)),
                element: .some(element),
                account: .some(item),
            ),
        ),
    )
}

/// Choose options of a select.
action select_page(
    p: Page,
    element: Text,
    values: List<Text>,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "select", element: .some(element), values: .some(values), frame: frame),
    )
    ask_page(p, request)
}

/// Choose the files of a file input, by absolute path.
action fill_page_files(
    p: Page,
    element: Text,
    paths: List<Text>,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "fill_files", element: .some(element), paths: .some(paths), frame: frame),
    )
    ask_page(p, request)
}

/// Press a key in the page, as `Control+a` or `Enter`.
action press_page(
    p: Page,
    key: Text,
    element: Option<Text> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(Ask(op: "press", key: .some(key), element: element, frame: frame))
    ask_page(p, request)
}

/// Send the element's form through the page's own validation.
action submit_page(
    p: Page,
    element: Text,
    cross_origin: Bool = false,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "submit", element: .some(element), cross_origin: cross_origin, frame: frame),
    )
    ask_page(p, request)
}

/// Keeps a page the page opened in a window of its own as a page of this browser, as its window
/// hands it over: a plain new tab, or a popup a window could not show beside the page.
action open_from_page(p: Page, url: Text, popup: Bool) {
    let url = normalized(url)
    if url != "" {
        let b = root Browser
        b.pages.insert(Page(url: url, title: ""))
    }
}

/// Writes the pages the page can go to one way into its record, as its window hands them over:
/// what the menu then offers beside Back and Forward.
action remember_history(p: Page, direction: Text, entries: Text) {
    let value = match json.parse(entries) {
        .ok(value) => value,
        .err(_) => {
            return
        }
    }
    let found = history_of(value, direction)
    if direction == "forward" {
        p.forward.remove_where(e -> true)
        for e in found {
            p.forward.insert(e)
        }
    } else {
        p.back.remove_where(e -> true)
        for e in found {
            p.back.insert(e)
        }
    }
}

/// Answers a whole form of the page in one ask: each question's answer by its name, through the
/// page's own validation, and the form's first button when `submit` is on. A form is named by
/// the shape `read_page_forms` publishes for it.
action fill_page_form(
    p: Page,
    form: Text,
    answers: Map<Text>,
    submit: Bool = false,
    cross_origin: Bool = false,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .pen_tool, label: "Answer a form") {
    let request = json.encode(
        Ask(
            op: "fill_form",
            form: .some(form),
            answers: .some(answers),
            submit: submit,
            cross_origin: cross_origin,
            frame: frame,
        ),
    )
    ask_page(p, request)
}

/// A trusted pointer gesture: press at the first point, move through the rest and release at
/// the last, for drawing on canvases.
action pointer_page(p: Page, steps: List<Text>, button: Text = "left") -> Answer uses page {
    let request = json.encode(Ask(op: "pointer", steps: .some(steps), button: .some(button)))
    ask_page(p, request)
}

/// Turn the page's wheel to the top or the bottom, into view, or by a distance.
action scroll_page(
    p: Page,
    element: Option<Text> = .none,
    to: Option<Text> = .none,
    by: Option<Int> = .none,
    into_view: Option<Text> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "scroll", element: element, to: to, by: by, into_view: into_view, frame: frame),
    )
    ask_page(p, request)
}

/// A picture of the page, or of one element's box, as a PNG.
action capture_page(
    p: Page,
    element: Option<Text> = .none,
    max_width: Option<Int> = .none,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .camera, label: "Capture the page") {
    let request = json.encode(
        Ask(op: "capture", element: element, max_width: max_width, frame: frame),
    )
    ask_page(p, request)
}

/// Open an address in the page's own window: a site named without its scheme opens as one
/// (`example.org`), and words that name no address open the search engine's answer to them.
action navigate(p: Page, url: Text) -> Answer uses page (
    icon: .arrow_right,
    label: "Go to an address",
) {
    let url = resolve(root Browser, url)
    let request = json.encode(Ask(op: "navigate", url: .some(url)))
    let answer = ask_page(p, request)
    p.url = url
    answer
}

/// What Enter in the address does: what follows an extension's keyword goes to the extension,
/// which hears of it and goes where it sends the page; anything else is an address to go to.
action submit_address(p: Page, typed: Text) -> Answer uses page {
    let b = root Browser
    match addressed_to(b, typed) {
        .some(a) => enter_omnibox(p, a.text),
        .none => navigate(p, typed),
    }
}

/// The person chose one of the rows under the address: the extension hears what it stands for.
action choose_omnibox(p: Page, content: Text) -> Answer uses page {
    enter_omnibox(p, content)
}

/// Go back to the page before this one.
action back(p: Page) -> Answer uses page (icon: .arrow_left, label: "Back") {
    let request = json.encode(Ask(op: "back"))
    ask_page(p, request)
}

/// Go forward to the page this one followed.
action forward(p: Page) -> Answer uses page (icon: .arrow_right, label: "Forward") {
    let request = json.encode(Ask(op: "forward"))
    ask_page(p, request)
}

/// Load the page again.
action reload(p: Page) -> Answer uses page (icon: .refresh, label: "Reload") {
    let request = json.encode(Ask(op: "reload"))
    ask_page(p, request)
}

/// Stop loading the page.
action stop_loading(p: Page) -> Answer uses page (icon: .x, label: "Stop") {
    let request = json.encode(Ask(op: "stop"))
    ask_page(p, request)
}

/// Leave the page's fullscreen: the engine stops presenting the page, and its window shows the
/// page with its own controls again at once, since the person pressed the way out. The engine's
/// own report of it follows into the record.
action leave_fullscreen(p: Page) -> Answer uses page (icon: .minimize, label: "Exit full screen") {
    p.fullscreen = false
    let request = json.encode(Ask(op: "exit_fullscreen"))
    ask_page(p, request)
}

/// Writes whether the page is in fullscreen into its record, as its window hands it over: a
/// video or a slide deck that filled the screen, or left it.
action remember_fullscreen(p: Page, fullscreen: Bool) {
    p.fullscreen = fullscreen
}

/// Show the page's article without its clutter.
action reader(p: Page, enabled: Bool) -> Answer uses page (icon: .book_open, label: "Reader view") {
    let request = json.encode(Ask(op: "reader", enabled: enabled))
    ask_page(p, request)
}

/// Show the page at `level` zoom, where 1 is actual size.
action zoom(p: Page, level: Float) -> Answer uses page (icon: .zoom_in, label: "Zoom") {
    let request = json.encode(Ask(op: "zoom", level: .some(level)))
    ask_page(p, request)
}

/// Find words in the page: the window's search is the page's find, and the words are kept so
/// the way on to the next match and back to the previous ask for the same ones. No words takes
/// the find away: the marks go, and the page is left as it was before they were typed.
action find_in_page(
    p: Page,
    text: Text,
    forward: Bool = true,
    next: Bool = false,
) -> Answer uses page {
    if text.trim() == "" {
        p.find_text = ""
        p.find_at = 0
        p.find_count = 0
        stop_finding(p)
    } else {
        p.find_text = text
        let request = json.encode(
            Ask(op: "find_in_page", text: .some(text), forward: forward, next: next),
        )
        ask_page(p, request)
    }
}

/// Close the find bar's search.
action stop_finding(p: Page) -> Answer uses page {
    let request = json.encode(Ask(op: "stop_finding"))
    ask_page(p, request)
}

/// Answer a page's own question: an alert, a confirmation or a prompt.
action answer_page_dialog(
    p: Page,
    id: Int,
    accept: Bool,
    text: Option<Text> = .none,
) -> Answer uses page {
    let request = json.encode(Ask(op: "answer_dialog", id: .some(id), accept: accept, text: text))
    ask_page(p, request)
}

/// Reads what the page waits on into its window's record: the question it asks, the menu it
/// showed, and how far it has loaded. The window calls it when it opens the page and after each
/// answer; nothing here asks the page anything of its own.
action refresh_page(p: Page) -> Answer uses page (
    icon: .refresh,
    label: "Check the page’s questions",
) {
    let request = json.encode(Ask(op: "asks"))
    let raw = page.call(request)
    let value = match json.parse(raw) {
        .ok(value) => value,
        .err(_) => {
            return Answer(message: raw)
        }
    }
    p.dialog = dialog_of(value)
    p.menu = menu_of(value)
    p.loading = json_flag(value, "loading")
    p.progress = json_number(value, "progress")
    // An engine that reports no zoom has none to tell: the page keeps the size it had.
    let asked_zoom = json_number(value, "zoom")
    if asked_zoom > 0.0 {
        p.zoom_level = asked_zoom
    }
    let found = json_field(value, "find") ?? value
    p.find_at = json_number(found, "active").to_int()
    p.find_count = json_number(found, "count").to_int()
    p.muted = json_flag(value, "muted")
    p.fullscreen = json_flag(value, "fullscreen")
    if let video = media_of(value) {
        p.media = .some(video)
    } else {
        p.media = .none
        // A page with no video has nothing to float.
        p.pip = false
    }
    p.site = site_of(value)
    let buttons = buttons_of(value)
    p.buttons.remove_where(b -> true)
    for b in buttons {
        p.buttons.insert(b)
    }
    let notices = notices_of(value)
    p.notices.remove_where(n -> true)
    for n in notices {
        p.notices.insert(n)
    }
    let before = history_of(value, "back")
    p.back.remove_where(e -> true)
    for e in before {
        p.back.insert(e)
    }
    let after = history_of(value, "forward")
    p.forward.remove_where(e -> true)
    for e in after {
        p.forward.insert(e)
    }
    Answer(ok: true, message: "read")
}

/// The page's window opened: it settles a private, a visitor's or a website account's page's engine
/// before anything is read, and opens any other signed in to the sites the person brought their
/// sign-ins of from a browser of theirs and lists their website accounts, then reads what the page
/// asks and shows before anyone looks. A window opened for an agent opens none of them, says so,
/// and the page browses as a visitor from then on, whichever window shows it.
opened action opened_page(p: Page) uses page {
    if not p.shared or p.visitor {
        let _ = page.call(json.encode(Ask(op: "private")))
    } else {
        let asked = if p.account != "" {
            Ask(op: "account", account: .some(p.account))
        } else { Ask(op: "sign_ins") }
        match json.parse(page.call(json.encode(asked))) {
            .ok(answer) => {
                if json_text(answer, "code") == "visitor" {
                    p.visitor = true
                }
            }
            .err(_) => {}
        }
        if p.account == "" and not p.visitor {
            let _ = page.call(json.encode(Ask(op: "accounts")))
        }
    }
    refresh_page(p)
}

/// Writes the person's website accounts that open in a window of their own, as their window's
/// vault listed them; a list the vault did not answer leaves them as they were. The window raises
/// it; a person, agent or script never does.
action remember_accounts(p: Page, accounts: Text, message: Text) {
    if message != "" {
        return
    }
    let b = root Browser
    let listed = website_accounts_of(accounts)
    b.accounts.remove_where(k -> not listed.any(a -> a.account == k.account and a.title == k.title))
    for a in listed {
        if not b.accounts.any(k -> k.account == a.account) { b.accounts.insert(a) }
    }
}

/// Writes whether the website account the page opens did, and why not. The window raises it; a
/// person, agent or script never does.
action remember_account(p: Page, open: Bool, message: Text) {
    p.account_notice = if open { "" } else { message }
}

/// Lets a site use what it asked for (its camera, microphone, location, notifications). The
/// window raises it as the Browser's own call when the site asks: the person allows it on the
/// system's card, which names the Browser and the site, and an always-allow lets this site have it
/// from then on. Declined, or the page gone, the site is told no.
destructive action allow_permission(
    p: Page,
    id: Int,
    origin: Text,
    wants: Text,
) -> Answer uses page (icon: .shield, label: "Allow the site", shows: [origin, wants]) {
    page_answered(page.call(json.encode(Ask(op: "answer_permission", id: .some(id), allow: true))))
}

/// Saves the file a page offers. The window raises it as the Browser's own call when the page
/// offers one, and the person allows it on the system's card.
destructive action save_download(p: Page, id: Int, name: Text, url: Text) -> Answer uses page (
    icon: .download,
    label: "Save the download",
    shows: [name, url],
) {
    page_answered(page.call(json.encode(Ask(op: "answer_download", id: .some(id), allow: true))))
}

/// Turns down the file a page offers: nothing is saved, and the page is told so.
action discard_download(p: Page, id: Int, name: Text = "") -> Answer uses page (
    icon: .x,
    label: "Do not save the download",
    shows: [name],
) {
    page_answered(page.call(json.encode(Ask(op: "answer_download", id: .some(id), allow: false))))
}

/// Downloads what an address serves into the person's Downloads, as a job of the Browser's own
/// that goes on when the window closes. A page's window raises it for a download the person
/// saved there, naming the handle its request waits under, and again for one the person
/// retried, naming the job it ended as (`again`); a person or agent names an address.
action download(
    p: Page,
    url: Text,
    name: Text = "",
    request: Text = "",
    again: Text = "",
) -> Answer (icon: .download, label: "Download an address") {
    require url.trim() != "" "Type the address to download"
    let address = url.trim()
    let b = root Browser
    let retried = b.downloads.find(d -> again != "" and retry_key(d) == again)
    if again != "" and retried == .none {
        return Answer(message: std.say("The download was retried already"))
    }
    let called = if let d = retried {
        d.name
    } else if name == "" { address.split("/").last() ?? address } else { name }
    match engine.start(
        "download",
        json.encode(DownloadAsk(url: address, name: name, request: request)),
    ) {
        .ok(said) => {
            let job = started_job(said)
            if let d = retried {
                d.job = job
                d.seen = .none
                d.engine = 0
            } else {
                b.downloads.insert(Download(url: address, name: called, job: job))
            }
            Answer(ok: true, message: std.say("Downloading {called}"))
        }
        .err(why) => Answer(message: why),
    }
}

/// The id of the job the engine answered starting with (`{"job": …}`), which its state is read
/// and it is stopped by.
fn started_job(said: Text) -> Text {
    match json.parse(said) {
        .ok(value) => json_text(value, "job"),
        .err(_) => said,
    }
}

/// Keeps where a download an extension started stands, as the page's engine, which saves it,
/// tells: the Browser's one list shows it with the rest. The window raises it as the download
/// moves; a person, agent or script never does.
action note_extension_download(
    p: Page,
    id: Int,
    state: Text,
    name: Text,
    url: Text,
    path: Text,
    received: Int,
    total: Int,
    error: Text,
) {
    let b = root Browser
    let now = DownloadJob(
        state: extension_download_state(state),
        done: received,
        total: total,
        path: path,
        name: name,
        error: error,
    )
    if let d = b.downloads.find(d -> d.engine == id and d.url == url and d.seen != .none) {
        d.seen = .some(now)
    } else {
        b.downloads.insert(Download(url: url, name: name, seen: .some(now), engine: id))
    }
}

/// The engine's word for where a download stands, as a job says it.
fn extension_download_state(state: Text) -> Text {
    if state == "progress" { "running" } else if state == "complete" { "done" } else { state }
}

/// Stops a download that is still going: what it saved so far is removed.
action cancel_download(d: Download) -> Answer (icon: .x, label: "Cancel the download") {
    require d.job != "" "The download has not started"
    if engine.cancel(d.job) {
        Answer(ok: true, message: std.say("Cancelled {d.name}"))
    } else {
        Answer(message: std.say("{d.name} is no longer downloading"))
    }
}

/// Starts a download that failed or was cancelled again, from the beginning, as signed in as the
/// page was: the page's window hands over what a request for the address carries from it now,
/// and the download keeps the new job.
action retry_download(p: Page, d: Download) -> Answer uses page (
    icon: .refresh,
    label: "Retry the download",
) {
    page_answered(
        page.call(
            json.encode(Ask(op: "download_again", url: .some(d.url), job: .some(retry_key(d)))),
        ),
    )
}

/// What names a download while its retry waits on the page: the job it ended as, or the page
/// engine's number for one an extension started, which has no job of the Browser's.
fn retry_key(d: Download) -> Text {
    if d.job != "" { d.job } else { "extension-{d.engine}" }
}

/// Stops every download still going, from every window.
action cancel_all_downloads() -> Answer (icon: .x, label: "Cancel every download") {
    let b = root Browser
    for d in b.downloads {
        if d.job != "" { engine.cancel(d.job) }
    }
    Answer(ok: true, message: std.say("Every download was stopped"))
}

/// Opens the engine's developer tools for this page: its console, its elements and its network.
action developer_tools(p: Page) -> Answer uses page (icon: .code, label: "Developer tools") {
    ask_page(p, json.encode(Ask(op: "developer_tools")))
}

/// Writes the page as a PDF and hands it to Print Centre, where the person chooses the printer
/// and how it prints. The engine answers with the file once it has written it; a page with
/// nothing open has nothing to print.
action print_page(p: Page) -> Answer uses page (icon: .printer, label: "Print", key: "Ctrl+P") {
    require p.url != "" "Open a page to print"
    ask_page(p, json.encode(Ask(op: "print")))
}

/// Hands the PDF the engine wrote to Print Centre, through its own action, so the person
/// chooses the printer and how it prints there; the copy the engine wrote goes. The window
/// raises it once the engine is done; a person, agent or script never does.
action note_print(p: Page, ok: Bool, path: Text, name: Text, message: Text) uses files {
    if not ok {
        p.print_notice = if message == "" {
            std.say("The page could not be printed")
        } else { message }
        return
    }
    let bytes = match File.read(path) {
        .ok(bytes) => bytes,
        .err(why) => {
            p.print_notice = std.say("The page could not be printed: {why}")
            return
        }
    }
    // A file an action keeps holds at most a megabyte, as a file a person picks does; a page
    // larger than that is told so rather than failing on the way to Print Centre.
    if bytes.len() > 1048576 {
        let _ = File.remove(path)
        p.print_notice = std.say("The page is too large to print")
        return
    }
    let called = if name.ends_with(".pdf") { name } else { std.say("{name}.pdf") }
    let file = File.of_bytes(if name == "" { "page.pdf" } else { called }, bytes)
    p.print_notice = ""
    let _ = File.remove(path)
    printing.print_file(printing.print_centre, file)
}

/// Opens a saved download in the app that opens its kind, through the system's one open.
action open_download(name: Text, path: Text) uses files (
    icon: .external_link,
    label: "Open the download",
) {
    require path != "" "Save the download first"
    match File.open(path) {
        .ok => Answer(ok: true, message: "Opened {name}"),
        .err(why) => Answer(message: why),
    }
}

/// Shows where a saved download stands in Files, with the file marked: the system's one place for
/// what a window saved.
action reveal_download(path: Text) (icon: .folder, label: "Show the download in Files") {
    require path != "" "Save the download first"
    files.show(files.files, path)
}

/// Silences the page's sound, or lets it play again.
action mute(p: Page, muted: Bool = not p.muted) -> Answer uses page (
    icon: .headphone_off,
    label: "Mute the page",
) {
    ask_page(p, json.encode(Ask(op: "mute", muted: muted)))
}

/// Floats the page's video above its window as picture-in-picture, so it plays while the person
/// works elsewhere, or puts it back where the page draws it.
action float_page_video(p: Page, floating: Bool = not p.pip) (
    icon: .picture_in_picture_2,
    label: "Picture in picture",
) {
    p.pip = floating
}

/// Asks the page where it can go back to. The engine answers with the pages before this one,
/// which the page's record holds once its window reads them again.
action read_back_history(p: Page) -> Answer uses page (icon: .arrow_left, label: "Earlier pages") {
    ask_page(p, json.encode(Ask(op: "history", direction: .some("back"))))
}

/// Asks the page where it can go forward to, the answer kept like [`read_back_history`]'s.
action read_forward_history(p: Page) -> Answer uses page (
    icon: .arrow_right,
    label: "Later pages",
) {
    ask_page(p, json.encode(Ask(op: "history", direction: .some("forward"))))
}

/// Goes to one page of the page's own history, by the entry its reading named.
action go_history_entry(p: Page, entry: Int) -> Answer uses page (
    icon: .arrow_right,
    label: "Go to a page of its history",
) {
    ask_page(p, json.encode(Ask(op: "history_entry", entry: .some(entry))))
}

/// Reads the site's connection, its certificate, the requests tracker protection blocked and
/// what the site may ask for, into the page's record.
action read_site(p: Page) -> Answer uses page (icon: .shield, label: "Site information") {
    require origin_of(p.url) != "" "This page is not on a site"
    ask_page(p, json.encode(Ask(op: "site_status", origin: .some(origin_of(p.url)))))
}

/// Clears this site's storage, cookies and remembered forms, including cookies it shares with
/// its subdomains, and reloads it.
destructive action clear_site_data(p: Page) -> Answer uses page (
    icon: .trash,
    label: "Clear this site's data",
) {
    require p.site != .none "Read the site's information first"
    if let site = p.site {
        return ask_page(p, site_action(site, "clear"))
    }
    Answer(message: "Read the site's information first")
}

/// Blocks what the site may ask for (its camera, microphone, location and notifications).
destructive action block_site_permissions(p: Page) -> Answer uses page (
    icon: .shield,
    label: "Block what this site asks for",
) {
    require p.site != .none "Read the site's information first"
    if let site = p.site {
        return ask_page(p, site_action(site, "revoke"))
    }
    Answer(message: "Read the site's information first")
}

/// Lets the site ask again for what it was refused.
action ask_site_permissions_again(p: Page) -> Answer uses page (
    icon: .refresh,
    label: "Let the site ask again",
) {
    require p.site != .none "Read the site's information first"
    if let site = p.site {
        return ask_page(p, site_action(site, "reset"))
    }
    Answer(message: "Read the site's information first")
}

/// Reads the login forms this page asks for.
action discover_logins(p: Page) -> Answer uses page (
    icon: .key,
    label: "Find this page’s logins",
) {
    ask_page(p, credential_request(p, Ask(credential: .some("discover"))))
}

/// Lists the logins the workspace's vault keeps for this site, by name.
action list_saved_logins(p: Page) -> Answer uses page (icon: .key, label: "Saved logins") {
    ask_page(p, credential_request(p, Ask(credential: .some("list"))))
}

/// Takes what the person typed into a login form, so it can be saved for this site. The password
/// stays in the engine until saving hands it to the vault, and never crosses to the app.
action capture_login(p: Page, f: LoginForm) -> Answer uses page (
    icon: .key,
    label: "Save this login",
) {
    ask_page(p, credential_request(p, Ask(credential: .some("capture"), form: .some(f.form))))
}

/// Saves the capture the engine took in the workspace's vault, as a login of this site the Browser
/// keeps.
action save_login(p: Page) -> Answer uses page (icon: .check, label: "Save the login") {
    require p.capture != "" "Take the login first"
    ask_page(p, credential_request(p, Ask(credential: .some("save"), capture: .some(p.capture))))
}

/// Fills a form of this page with a login the vault keeps for this site. A login the Browser saved
/// itself fills at once; any other is filled once its person allowed the Browser that one login.
action fill_saved_login(p: Page, f: LoginForm, a: LoginAccount) -> Answer uses page (
    icon: .key,
    label: "Fill the saved login",
) {
    ask_page(
        p,
        credential_request(
            p,
            Ask(credential: .some("fill"), form: .some(f.form), account: .some(a.account)),
        ),
    )
}

/// Replaces a login the vault keeps with the capture the engine took.
action update_saved_login(p: Page, f: LoginForm, a: LoginAccount) -> Answer uses page (
    icon: .refresh,
    label: "Update the saved login",
) {
    require p.capture != "" "Take the login first"
    ask_page(
        p,
        credential_request(
            p,
            Ask(
                credential: .some("update"),
                form: .some(f.form),
                account: .some(a.account),
                capture: .some(p.capture),
            ),
        ),
    )
}

/// Forgets a login of this site from the vault: one the Browser saved, or one it may fill.
destructive action delete_saved_login(p: Page, a: LoginAccount) -> Answer uses page (
    icon: .trash,
    label: "Forget the saved login",
) {
    ask_page(p, credential_request(p, Ask(credential: .some("delete"), account: .some(a.account))))
}

/// Turns down the login the engine offers to save.
action cancel_login(p: Page) -> Answer uses page (icon: .x, label: "Do not save the login") {
    ask_page(p, credential_request(p, Ask(credential: .some("cancel"))))
}

/// Reads the addresses the engine fills forms with.
action discover_addresses(p: Page) -> Answer uses page (
    icon: .user,
    label: "Addresses this page’s forms take",
) {
    ask_page(p, credential_request(p, Ask(credential: .some("addresses"))))
}

/// Fills a form of this page with a saved address, through the engine.
action fill_saved_address(p: Page, f: LoginForm, a: Address) -> Answer uses page (
    icon: .user,
    label: "Fill with a saved address",
) {
    ask_page(
        p,
        credential_request(
            p,
            Ask(credential: .some("fill_address"), form: .some(f.form), address: .some(a)),
        ),
    )
}

/// Reads the extensions installed on this device into the Browser's record.
action list_extensions(p: Page) -> Answer uses page (
    icon: .puzzle,
    label: "Read the installed extensions",
) {
    ask_page(p, json.encode(Ask(op: "extension_list")))
}

/// Installs the extension package the person chose, a .crx signed by its publisher or a plain
/// .zip: the engine verifies a signed package, unpacks it under its own identifier, and lists
/// it. The person picks the package with the system's own chooser, as any app's file.
action install_extension(p: Page, file: File) -> Answer uses page (
    icon: .file_plus,
    label: "Install an extension from a file",
) {
    ask_page(p, json.encode(Ask(op: "extension_install", file: .some(file.url))))
}

/// Installs the Chrome Web Store's item `said` (a store page's address, or the extension's id):
/// its package comes from the store's update service, is verified, and is turned on as the
/// store installs one.
action install_store_extension(p: Page, said: Text) -> Answer uses page (
    icon: .puzzle,
    label: "Add from the Chrome Web Store",
    shows: [said],
) {
    require said.trim() != "" "Name the extension or its store page"
    ask_page(p, json.encode(Ask(op: "extension_install_store", item: .some(said.trim()))))
}

/// Asks the Chrome Web Store for newer versions of the store extensions installed here and
/// installs them: they are there the next time a page opens, and are loaded now.
action update_extensions(p: Page) -> Answer uses page (
    icon: .refresh,
    label: "Check the extensions for updates",
) {
    ask_page(p, json.encode(Ask(op: "extension_update_check")))
}

/// Turns an installed extension on, or off, in the running window: Chromium loads and unloads
/// it where the page stands, so nothing waits for a window restart.
action enable_extension(p: Page, e: Extension, on: Bool = not e.enabled) -> Answer uses page (
    icon: .check,
    label: "Turn the extension on or off",
    shows: [e.name],
) {
    e.enabled = on
    ask_page(p, json.encode(Ask(op: "extension_enable", extension: .some(e.key), enabled: on)))
}

/// Forgets an extension installed on this device: its folder and everything it may do.
destructive action remove_extension(p: Page, e: Extension) -> Answer uses page (
    icon: .trash,
    label: "Remove the extension",
    shows: [e.name],
) {
    ask_page(p, json.encode(Ask(op: "extension_remove", extension: .some(e.key))))
}

/// Presses one of the page's extension buttons: the engine runs the extension's action, or opens
/// the page it offers.
action click_extension(p: Page, b: PageButton) -> Answer uses page (
    icon: .puzzle,
    label: "Press the extension",
    shows: [b.label],
) {
    require b.enabled "The extension turned its button off on this page"
    ask_page(p, json.encode(Ask(op: "extension_click", extension: .some(b.ext))))
}

/// Presses one of the buttons a notification an extension showed offers.
action choose_extension_notice(p: Page, n: ExtensionNotice, button: Text) -> Answer uses page (
    icon: .check,
    label: "Press the notification's button",
    shows: [n.title],
) {
    ask_page(
        p,
        json.encode(
            Ask(
                op: "extension_notice_chosen",
                extension: .some(n.ext),
                notification: .some(n.notification),
                button: .some(button),
            ),
        ),
    )
}

/// Takes a notification an extension showed away without pressing anything.
action dismiss_extension_notice(p: Page, n: ExtensionNotice) -> Answer uses page (
    icon: .x,
    label: "Take the notification away",
    shows: [n.title],
) {
    ask_page(
        p,
        json.encode(
            Ask(
                op: "extension_notice_chosen",
                extension: .some(n.ext),
                notification: .some(n.notification),
            ),
        ),
    )
}

/// Writes the extensions a page's window listed into the Browser's record. The window raises it;
/// a person, agent or script never does.
action remember_extensions(p: Page, installed: Text, message: Text) {
    let b = root Browser
    let found = extensions_of(installed)
    b.extensions.remove_where(e -> true)
    for e in found {
        b.extensions.insert(e)
    }
    if message != "" {
        b.extension_notice = message
    }
}

/// Writes what installing, turning on or removing an extension came to, in the person's words.
/// The window raises it; a person, agent or script never does.
action note_extension(p: Page, ok: Bool, kind: Text, message: Text, id: Text) {
    let b = root Browser
    b.extension_notice = if ok { "" } else { extension_trouble(kind, message) }
}

/// Asks the extension for its side panel beside the page: its own page says what the panel shows,
/// and the page's window shows it there.
action open_side_panel(p: Page, e: Extension) -> Answer uses page (
    icon: .panel_right,
    label: "Open the extension’s side panel",
    shows: [e.name],
) {
    require e.enabled "The extension is off"
    require e.side_panel "The extension has no side panel"
    ask_page(p, json.encode(Ask(op: "open_side_panel", extension: .some(e.key))))
}

/// Closes the page the page's window showed beside or over this one: an extension's side panel,
/// the page it asked the person to sign in on, or a popup a script opened.
action close_panel(p: Page) -> Answer uses page (
    icon: .x,
    label: "Close the page beside this one",
) {
    ask_page(p, json.encode(Ask(op: "panel_close")))
}

/// Writes the page the page's window shows beside this one into its record, or takes it away.
/// The window raises it; a person, agent or script never does.
action remember_panel(p: Page, open: Bool, kind: Text, id: Text, url: Text, interactive: Bool) {
    p.panel = if open {
        .some(Panel(kind: kind, ext: id, url: url, interactive: interactive))
    } else { .none }
}

/// Writes the extension the address addresses, and the rows it offers, into the page's record; an
/// empty identifier takes them away, since the address no longer addresses one. The window
/// raises it; a person, agent or script never does.
action remember_omnibox(p: Page, id: Text, name: Text, text: Text, rows: Text) {
    p.omnibox = if id == "" {
        .none
    } else {
        .some(Omnibox(ext: id, name: name, text: text, rows: omnibox_rows(rows)))
    }
}

/// What the person typed in the page's address: when it starts with an extension's keyword and a
/// space, what follows is the extension's, which hears of each change.
action typed_address(p: Page, typed: Text) -> Answer uses page {
    let b = root Browser
    let request = match addressed_to(b, typed) {
        .some(a) => json.encode(
            Ask(
                op: "address_typed",
                text: .some(typed),
                extension: .some(a.ext),
                name: .some(a.name),
                rest: .some(a.text),
            ),
        ),
        .none => json.encode(Ask(op: "address_typed", text: .some(typed))),
    }
    page_answered(page.call(request))
}

/// The person left the address without Enter: the extension hears of it.
agents action address_left(p: Page) -> Answer uses page {
    page_answered(page.call(json.encode(Ask(op: "address_left"))))
}

/// The person pressed Enter, or chose one of the rows: the extension hears what it was given.
action enter_omnibox(p: Page, text: Text) -> Answer uses page {
    page_answered(page.call(json.encode(Ask(op: "address_entered", text: .some(text)))))
}

/// Writes what asking the Chrome Web Store for newer versions came to, in the person's words.
/// The window raises it; a person, agent or script never does.
action note_extension_updates(p: Page, checked: Int, updated: Text, failed: Text) {
    let b = root Browser
    let news = match json.parse(updated) {
        .ok(value) => json_arrays(value),
        .err(_) => [],
    }
    if let first = news.at(0) {
        if news.len() == 1 {
            let name = json_text(first, "name")
            let version = json_text(first, "version")
            b.extension_notice = std.say("“{name}” updated to {version}")
        } else {
            let count = news.len()
            b.extension_notice = std.say("{count} extensions were updated")
        }
    } else if failed != "" {
        b.extension_notice = std.say("The Chrome Web Store could not be checked: {failed}")
    } else if checked == 0 {
        b.extension_notice = std.say("No extension from the Chrome Web Store is installed")
    } else {
        b.extension_notice = std.say("The extensions are up to date")
    }
}

/// Writes what a login action came to into the page's record: the forms the engine found, the
/// vault's logins for the site, the capture the engine took, and their own words. The window
/// raises it; a person, agent or script never does.
action remember_credential(
    p: Page,
    did: Text,
    success: Bool,
    message: Text,
    forms: Text,
    accounts: Text,
    capture: Text,
    username: Text,
    late: Bool,
) {
    p.login_notice = message
    if not success {
        return
    }
    if did == "discover" {
        let found = login_forms_of(forms)
        p.login_forms.remove_where(f -> true)
        for f in found {
            p.login_forms.insert(f)
        }
    } else if did == "list" or did == "delete" {
        let found = login_accounts_of(accounts)
        p.login_accounts.remove_where(a -> true)
        for a in found {
            p.login_accounts.insert(a)
        }
    } else if did == "capture" {
        if success {
            p.capture = capture
        }
    } else if did == "save" or did == "update" {
        if success {
            p.capture = ""
        }
    }
}

/// What a site action asks the engine: the site, the document it was read from, and what to do.
fn site_action(site: SiteInfo, doing: Text) -> Text {
    json.encode(
        Ask(
            op: "site_action",
            origin: .some(site.origin),
            generation: .some(site.generation),
            doing: .some(doing),
        ),
    )
}

/// Tells the engine which sites tracker protection is off for, as the Browser's own set says:
/// the engine keeps the same list beside its filters.
action sync_site_exceptions(p: Page) -> Answer uses page (
    icon: .shield,
    label: "Apply the sites tracker protection is off for",
) {
    ask_page(
        p,
        json.encode(
            Ask(op: "site_exceptions", origins: .some(protection_exceptions(root Browser))),
        ),
    )
}

/// The sites tracker protection is off for, in the order the Browser holds them.
fn protection_exceptions(b: Browser) -> List<Text> {
    b.sites_without_protection.filter(site -> true)
}

/// Asks for the login action `ask` on this page. The window names the document the person is
/// looking at and the site its engine says the page is at, so an action never lands on a page
/// that came after it, and the vault is asked only for that site.
fn credential_request(p: Page, ask: Ask) -> Text {
    json.encode(
        Ask(
            op: "credential",
            credential: ask.credential,
            origin: .some(origin_of(p.url)),
            form: ask.form,
            account: ask.account,
            capture: ask.capture,
            address: ask.address,
        ),
    )
}

/// The login forms a credential answer listed.
fn login_forms_of(forms: Text) -> List<LoginForm> {
    let value = match json.parse(forms) {
        .ok(value) => value,
        .err(_) => {
            return []
        }
    }
    let found: List<LoginForm> = []
    for held in json_arrays(value) {
        let form = json_text(held, "form")
        if form != "" {
            found = [
                found,
                [
                    LoginForm(
                        form: form,
                        username: json_text(held, "username"),
                        kind: json_text(held, "kind"),
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    found
}

/// The logins of the vault a login answer listed.
fn login_accounts_of(accounts: Text) -> List<LoginAccount> {
    let value = match json.parse(accounts) {
        .ok(value) => value,
        .err(_) => {
            return []
        }
    }
    let found: List<LoginAccount> = []
    for held in json_arrays(value) {
        let account = json_text(held, "account")
        if account != "" {
            let login = LoginAccount(
                account: account,
                username: json_text(held, "username"),
                title: json_text(held, "title"),
                own: json_bool(held, "own"),
            )
            found = [found, [login]].flat_map(x -> x)
        }
    }
    found
}

/// Gives an extension the more access it asked for. The window raises it as the Browser's own
/// call, and the person allows it on the system's card, which says what the extension could then
/// do.
destructive action allow_extension_access(
    p: Page,
    id: Int,
    extension: Text,
    could: Text,
) -> Answer uses page (icon: .puzzle, label: "Allow the extension", shows: [extension, could]) {
    page_answered(page.call(json.encode(Ask(op: "answer_extension", id: .some(id), allow: true))))
}

/// Signs in to a site that asks for a user name and a password. The window raises it as the
/// Browser's own call; the person types both on the system's card, on this device, and the
/// password goes to the site alone: never into this action's arguments, its record or history.
destructive action sign_in(
    p: Page,
    id: Int,
    asker: Text,
    site: Text,
    realm: Text,
) -> Answer uses page (
    icon: .key,
    label: "Sign in to the site",
    shows: [site, realm],
    takes: ["User name", ("Password", .secret)],
) {
    page_answered(
        page.call(
            json.encode(
                Ask(op: "answer_sign_in", id: .some(id), asker: .some(asker), typed: true),
            ),
        ),
    )
}

/// Turns down a site's sign-in: nothing is sent, and the server shows its own page instead.
action cancel_sign_in(p: Page, id: Int, asker: Text) -> Answer uses page (
    icon: .x,
    label: "Cancel the sign-in",
) {
    page_answered(
        page.call(json.encode(Ask(op: "answer_sign_in", id: .some(id), asker: .some(asker)))),
    )
}

/// Lets what the page sends leave it: a form, a navigation or a request kept until the person
/// says so. The window raises it as the Browser's own call, and the person allows it on the
/// system's card, which says what leaves and where. What leaves is not taken back, so the
/// person's own press confirms it first too.
destructive action let_leave(
    p: Page,
    id: Int,
    origin: Text,
    method: Text,
    leaving: Text,
) -> Answer uses page (icon: .send, label: "Let it leave", shows: [leaving, origin, method]) {
    page_answered(
        page.call(
            json.encode(Ask(op: "answer_held_request", id: .some(id), allow: true)),
            outside: true,
        ),
    )
}

/// Takes a row of the page's own menu, by the command the engine gave it.
action context_menu_action(p: Page, id: Int, command: Int) -> Answer uses page {
    let request = json.encode(
        Ask(op: "context_menu_action", id: .some(id), command: .some(command)),
    )
    ask_page(p, request)
}

/// Saves a page for later. A page is bookmarked by its address alone: bookmarking it twice keeps
/// one bookmark, and no title given falls back to the address.
action bookmark(b: Browser, url: Text, title: Option<Text> = .none) (
    icon: .star,
    label: "Bookmark a page",
) {
    require normalized(url) != "" "A bookmark is an absolute http(s) address"
    require b.bookmarks.find(k -> k.url == normalized(url)) == .none "That page is bookmarked already"
    let url = normalized(url)
    let said = title ?? ""
    let title = if said.trim() == "" { url } else { said.trim() }
    b.bookmarks.insert(Bookmark(url: url, title: title))
}

/// Takes a bookmark back.
action remove_bookmark(k: Bookmark) (icon: .trash, label: "Remove bookmark") {
    let b = root Browser
    b.bookmarks.remove(k)
}

/// Renames a saved page, when the person likes their own name for it better than its title.
action name_bookmark(k: Bookmark, title: Text) (icon: .pen_tool, label: "Name this bookmark") {
    require title.trim() != "" "A bookmark is called by some name"
    k.title = title.trim()
}

/// Puts a bookmark in a folder of the person's library, such as `Work/Reading`, or at the top
/// of the bookmarks when the folder is empty.
action move_bookmark(k: Bookmark, folder: Text) (
    icon: .folder,
    label: "Put the bookmark in a folder",
) {
    k.folder = folder.trim()
}

/// Turns tracker protection off for one site, or back on. A site is named by its origin, so
/// turning it off for `https://example.org` covers every page of it.
action set_site_protection(b: Browser, origin: Text, enabled: Bool) (
    icon: .shield,
    label: "Tracker protection for a site",
) {
    require origin_of(origin) != "" "A site is named by one http(s) origin"
    let site = origin_of(origin)
    if enabled {
        b.sites_without_protection.remove(site)
    } else { b.sites_without_protection.add(site) }
}

/// Turns tracker protection off for this site, or back on, as the page's shield does.
action toggle_site_protection(b: Browser, origin: Text) (
    icon: .shield,
    label: "Protect this site",
) {
    set_site_protection(b, origin, b.sites_without_protection.contains(origin_of(origin)))
}

/// Forgets one form a site asked.
action forget_form(f: RememberedForm) (icon: .trash, label: "Forget this form") {
    let b = root Browser
    b.forms.remove(f)
}

/// Forgets every form a site asked.
action forget_site_forms(b: Browser, origin: Text) (
    icon: .trash,
    label: "Forget this site’s forms",
) {
    require origin_of(origin) != "" "A site is named by one http(s) origin"
    let site = origin_of(origin)
    b.forms.remove_where(f -> f.origin == site)
}

/// Forgets every form every site asked.
action forget_all_forms(b: Browser) (icon: .trash, label: "Forget all remembered forms") {
    b.forms.remove_where(f -> true)
}

/// Remembers what a page's forms ask, as its window reads them: the shape of each form and of its
/// questions, never what a field held. A form read here already is read again: what it asks now
/// replaces what it asked before. `questions` is one question a line, its fields a tab apart:
/// name, the kind of answer, the label, `true` when it must be answered, and the answers it
/// offers, a `|` between them.
action remember_form(
    b: Browser,
    origin: Text,
    signature: Text,
    label: Text,
    page: Text,
    questions: List<Text>,
    whole: Bool,
) uses time {
    require origin_of(origin) != "" "A form is remembered of one http(s) origin"
    require signature != "" "A form is named by its shape"
    let site = origin_of(origin)
    let asked = questions.map(line -> question_of(line)).filter(q -> q.name != "")
    let form = match b.forms.find(f -> f.origin == site and f.signature == signature) {
        .some(held) => {
            held.read_at = now
            if label != "" {
                held.label = label
            }
            if not held.pages.contains(page) {
                held.pages = [held.pages, [page]].flat_map(x -> x)
            }
            held
        }
        .none => b.forms.insert(
            RememberedForm(
                origin: site,
                signature: signature,
                label: label,
                pages: [page],
                read_at: now,
            ),
        ),
    }
    form.seen += 1
    form.questions.remove_where(q -> true)
    for q in asked {
        form.questions.insert(q)
    }
    if whole {
        // A page that says these are every form it asks now: a form it no longer asks, read on
        // this page before, is forgotten.
        b.forms.remove_where(
            held -> held.origin == site
                and held.pages.contains(page)
                and held.signature != signature,
        )
    }
}

/// Adds an address for the forms the person fills.
action add_address(b: Browser) -> Address (icon: .user_plus, label: "Add an address", key: "n") {
    b.addresses_enabled = true
    b.addresses.insert(Address(label: "", full_name: ""))
}

/// Deletes an address, and stops filling forms with it.
action delete_address(a: Address) (icon: .trash, label: "Delete address") {
    let b = root Browser
    b.addresses.remove(a)
}

/// Fills a form of the page with a saved address, question by question, without sending it. The
/// form is named by the shape its reading publishes (`RememberedForm.signature`, or the
/// signature `read_page_forms` answers), and the page's own validation still decides whether it
/// could be sent.
action fill_address(p: Page, form: Text, a: Address) -> Answer uses page (
    icon: .user,
    label: "Fill the form with an address",
) {
    require not address_answers(forms_named(root Browser, form), a).keys().is_empty() "This form asks nothing an address answers"
    let answers = address_answers(forms_named(root Browser, form), a)
    ask_page(p, json.encode(Ask(op: "fill_form", form: .some(form), answers: .some(answers))))
}

/// What `a` answers the questions of a remembered form with, by each question's name: the ones
/// an address answers nothing for are left out, so the page reads them as untouched.
fn address_answers(questions: List<Question>, a: Address) -> Map<Text> {
    let answers = Map<Text>()
    for q in questions {
        let answer = address_answer(a, q.name)
        if answer != "" { answers.put(q.name, answer) }
    }
    answers
}

/// The questions the remembered form `signature` asks, empty when no site's form has that shape.
fn forms_named(b: Browser, signature: Text) -> List<Question> {
    match b.forms.find(f -> f.signature == signature) {
        .some(f) => f.questions.filter(q -> true),
        .none => [],
    }
}

/// What an address answers a form's question with, read from the question's own name, or
/// nothing when its name answers no part of an address.
fn address_answer(a: Address, question: Text) -> Text {
    let name = question.lower().replace("_", "").replace("-", "").replace(" ", "")
    if name == "name" or name == "fullname" or name == "yourname" {
        return a.full_name
    }
    if name == "givenname" or name == "firstname" {
        return a.given_name
    }
    if name == "familyname" or name == "lastname" or name == "surname" {
        return a.family_name
    }
    if name == "street" or name == "streetaddress" or name == "address" or name == "address1" {
        return a.street
    }
    if name == "city" or name == "town" {
        return a.city
    }
    if name == "region" or name == "state" or name == "province" {
        return a.region
    }
    if name == "postalcode" or name == "postcode" or name == "zip" or name == "zipcode" {
        return a.postal_code
    }
    if name == "country" or name == "countrycode" {
        return a.country
    }
    ""
}

/// Names a visit, when the person likes their own name for a page better than its title.
action name_visit(v: Visit, title: Text) (icon: .pen_tool, label: "Name this visit") {
    require title.trim() != "" "A visit is called by some name"
    v.title = title.trim()
}

/// Forgets one visit. A bookmark of the page stays.
action forget_visit(v: Visit) (icon: .trash, label: "Forget this page") {
    let b = root Browser
    b.visits.remove(v)
}

/// Clears the browsing history. Bookmarks and remembered forms stay.
action clear_history(b: Browser) (icon: .trash, label: "Clear browsing history") {
    b.visits.remove_where(v -> true)
    b.imported_visits.remove_where(v -> true)
}

/// Adds a page a window opened to the person's history, counting it when it is there already.
/// A page that opens itself is a visit like any other.
action visit(b: Browser, url: Text, title: Text, at: Time) {
    let url = normalized(url)
    if url == "" {
        return
    }
    let said = title.trim()
    if let v = b.visits.find(x -> x.url == url) {
        v.last = at
        v.count += 1
        if said != "" {
            v.title = said
        }
    } else {
        b.visits.insert(Visit(url: url, title: if said == "" { url } else { said }, last: at))
    }
    trim_history(b, 10000)
}

/// Keeps the person's own visits to their newest `most`, forgetting the oldest: the history stays
/// within what a person could read, as every other list the system keeps does.
action trim_history(b: Browser, most: Int) {
    require most > 0 "A history keeps at least one page"
    let all = b.visits.filter(v -> true).sort_by(v -> v.last)
    if all.len() > most {
        for v in all.take(all.len() - most) {
            b.visits.remove(v)
        }
    }
}

/// Takes in a page a window arrived at: it joins the person's history.
action arrived_at(b: Browser, url: Text) uses time {
    visit(b, url, "", now)
}

/// The page went somewhere: where it is now is written on the page, and the visit joins the
/// person's history unless the page's window paused it.
action went_to(p: Page, url: Text) uses time {
    let url = normalized(url)
    if url == "" {
        return
    }
    // A video floats the page it is on: another document draws another picture.
    if url != p.url {
        p.pip = false
    }
    p.url = url
    if not p.paused and p.shared { arrived_at(root Browser, url) }
}

/// Pauses or resumes the person's history in a page's window.
action toggle_pause(p: Page, paused: Bool = not p.paused) (icon: .clock, label: "Pause history") {
    p.paused = paused
}

/// Takes in the browsing data another browser holds, from the bundle this app's own import and
/// `tilo migrate` hand over: its history, its bookmarks, or both.
action import_browsing(b: Browser, file: File) (
    icon: .file_plus,
    label: "Import browsing data",
    opens: ["application/json", .json],
) {
    let text = match file.text() {
        .ok(text) => text,
        .err(why) => {
            b.trouble = why
            return
        }
    }
    import_history(b, text)
    import_bookmarks(b, text)
}

/// Imports the pages another browser's history holds, from the bundle this app's own import and
/// `tilo migrate` hand over: a JSON text of `{"browser": "...", "visits": [{"url": "...",
/// "title": "...", "last": 1760000000, "count": 1}]}`. Importing the same bundle twice keeps one
/// set of pages.
action import_history(b: Browser, bundle: Text) (
    icon: .file_plus,
    label: "Import browsing history",
) {
    let value = match json.parse(bundle) {
        .ok(value) => value,
        .err(why) => {
            b.trouble = why
            return
        }
    }
    let visits_field = json_field(value, "visits")
    let pages = if visits_field == .none {
        json_field(value, "pages") ?? value
    } else { visits_field ?? value }
    let taken = 0
    for page in json_arrays(pages) {
        let url = normalized(json_text(page, "url"))
        if url != "" {
            let title = json_text(page, "title")
            let last = epoch(json_number(page, "last"))
            let count = json_number(page, "count").to_int()
            if let held = b.imported_visits.find(v -> v.url == url) {
                held.last = last
                held.count += count
                if title != "" {
                    held.title = title
                }
            } else {
                b.imported_visits.insert(
                    ImportedVisit(
                        url: url,
                        title: if title == "" { url } else { title },
                        last: last,
                        count: count,
                    ),
                )
            }
            taken += 1
        }
    }
    b.trouble = if taken == 0 { std.say("This history holds no page") } else { "" }
}

/// Imports the bookmarks another browser holds, from the bundle this app's own import hands
/// over: the `ImportBundle` JSON with bookmark chunks (`{"bookmarks": [{"url": "...",
/// "title": "...", "folder": "Work"}]}`). Importing the same file twice keeps one bookmark of
/// each page.
action import_bookmarks(b: Browser, bundle: Text) (icon: .file_plus, label: "Import bookmarks") {
    let found = bookmarks_of_json(bundle.trim())
    if found.is_empty() {
        b.trouble = std.say("This file holds no bookmark")
        return
    }
    for made in found {
        if let held = b.bookmarks.find(k -> k.url == made.url) {
            if held.title == "" or held.title == held.url {
                held.title = made.title
            }
        } else { b.bookmarks.insert(made) }
    }
    b.trouble = ""
}

/// The page the address bar opens for what was typed: the address itself when it is one, the
/// address as a site when it looks like one (`example.org`), and the search engine's answer
/// otherwise.
fn resolve(b: Browser, typed: Text) -> Text {
    let said = typed.trim()
    let lower = said.lower()
    if said == "" or lower.contains("://") and not lower.contains(" ") {
        said
    } else if looks_like_site(lower) { "https://{said}" } else { search_address(b, said) }
}

/// The search engine's answer to `words`: the address it is asked at.
fn search_address(b: Browser, words: Text) -> Text {
    engine(b).replace("%s", percent(words.trim()))
}

/// The search engine the person chose, as the address whose `%s` the words replace.
fn engine(b: Browser) -> Text {
    if b.search_engine.contains("%s") { b.search_engine } else { "https://duckduckgo.com/?q=%s" }
}

/// Whether what was typed names a site: a name with a dot and no space in it, such as
/// `example.org`, or the device itself.
fn looks_like_site(typed: Text) -> Bool {
    typed != ""
        and not typed.contains(" ")
        and (typed.contains(".") or typed.starts_with("localhost"))
}

/// The address as this app keeps it: trimmed, with the scheme put in front when it has none.
/// Empty for anything that is not an http(s) address.
fn normalized(url: Text) -> Text {
    let said = url.trim()
    let lower = said.lower()
    if lower.starts_with("http://") or lower.starts_with("https://") {
        said
    } else if looks_like_site(lower) { "https://{said}" } else { "" }
}

/// The origin of a site's address: scheme, host and port, without a path. Empty for anything
/// that is not an http(s) address.
fn origin_of(url: Text) -> Text {
    let said = normalized(url)
    if said == "" {
        return ""
    }
    let parts = said.split("://")
    let scheme = parts.at(0) ?? "https"
    let after = parts.at(parts.len() - 1) ?? ""
    let host = after.split("/").at(0) ?? ""
    if host == "" { "" } else { "{scheme}://{host}" }
}

/// What the words type into a search engine's query: the characters that would end it written
/// so the engine reads them as part of it.
fn percent(words: Text) -> Text {
    words
        .replace("%", "%25")
        .replace("&", "%26")
        .replace("?", "%3F")
        .replace("#", "%23")
        .replace("+", "%2B")
        .replace(" ", "+")
}

/// The question a line of a form's reading says, each field a tab apart.
fn question_of(line: Text) -> Question {
    let fields = line.split("\t")
    let name = fields.at(0) ?? ""
    Question(
        name: name,
        answer: fields.at(1) ?? "text",
        label: fields.at(2) ?? name,
        required: (fields.at(3) ?? "") == "true",
        choices: (fields.at(4) ?? "").split("|").filter(c -> c != ""),
    )
}

/// The bookmarks a JSON bundle holds: a flat list, each with its folder, or another browser's
/// tree of folders with their children. A nested folder's bookmarks keep their path
/// (`Work/Reading`).
fn bookmarks_of_json(text: Text) -> List<Bookmark> {
    let value = match json.parse(text) {
        .ok(value) => value,
        .err(why) => {
            return []
        }
    }
    bookmarks_in(json_field(value, "bookmarks") ?? value, "")
}

/// The bookmarks `entries` holds, under the folder `under` when they stand in one.
fn bookmarks_in(entries: json.Value, under: Text) -> List<Bookmark> {
    let found: List<Bookmark> = []
    for entry in json_arrays(entries) {
        let url = normalized(json_text(entry, "url"))
        let title = json_text(entry, "title")
        if url != "" {
            let folder = if under != "" { under } else { json_text(entry, "folder") }
            found = [
                found,
                [Bookmark(url: url, title: if title == "" { url } else { title }, folder: folder)],
            ]
                .flat_map(x -> x)
        } else if let children = json_field(entry, "children") {
            let named = if under == "" {
                title
            } else if title == "" { under } else { "{under}/{title}" }
            found = [found, bookmarks_in(children, named)].flat_map(x -> x)
        }
    }
    found
}

/// The value at `key` of a JSON object, and nothing for anything else.
fn json_field(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The entries of a JSON array, one value as an array of itself, and nothing for an object.
fn json_arrays(value: json.Value) -> List<json.Value> {
    match value {
        .object(fields) => [],
        .array(items) => items,
        .string(text) => [value],
        .number(number) => [value],
        .bool(flag) => [value],
        .null => [],
    }
}

/// The text at `key` of a JSON object, and nothing when it holds none there.
fn json_text(value: json.Value, key: Text) -> Text {
    match json_field(value, key) {
        .some(held) => match held {
            .object(fields) => "",
            .array(items) => "",
            .string(text) => text,
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
        .none => "",
    }
}

/// Whether `key` of a JSON object holds `true`.
fn json_bool(value: json.Value, key: Text) -> Bool {
    match json_field(value, key) {
        .some(held) => match held {
            .object(fields) => false,
            .array(items) => false,
            .string(text) => false,
            .number(number) => false,
            .bool(flag) => flag,
            .null => false,
        },
        .none => false,
    }
}

/// The number at `key` of a JSON object, and none when it holds none there.
fn json_number(value: json.Value, key: Text) -> Float {
    match json_field(value, key) {
        .some(held) => match held {
            .object(fields) => 0.0,
            .array(items) => 0.0,
            .string(text) => 0.0,
            .number(number) => number,
            .bool(flag) => 0.0,
            .null => 0.0,
        },
        .none => 0.0,
    }
}

/// The page `p` does what `request` (JSON, as the window's page reads it) says, and answers the
/// page's own report. What was done is written into the page's record; a call that could not be
/// made answers with its words. Every named page action below is this one, written out.
action ask_page(p: Page, request: Text) -> Answer uses page (icon: .send, label: "Ask the page") {
    let raw = page.call(request)
    let op = match json.parse(request) {
        .ok(value) => json_text(value, "op"),
        .err(_) => "",
    }
    let value = match json.parse(raw) {
        .ok(value) => value,
        .err(_) => {
            p.steps.insert(PageStep(at: 0, operation: op, ok: false, message: raw))
            return Answer(message: raw)
        }
    }
    let ok = json_flag(value, "ok")
    // A find puts the element it found in its findings, not in the reading.
    let found = json_field(value, "data") ?? value
    let answer = Answer(
        ok: ok,
        code: json_text(value, "code"),
        message: json_text(value, "message"),
        url: json_text(value, "url"),
        title: json_text(value, "title"),
        navigation: json_text(value, "navigation"),
        loading: json_flag(value, "loading"),
        outline: json_text(value, "outline"),
        element: json_text(found, "element"),
        line: json_number(found, "line").to_int(),
        role: json_text(found, "role"),
        name: json_text(found, "name"),
        frame: json_text(found, "frame"),
        image: json_text(value, "image"),
    )
    p.steps.insert(
        PageStep(
            at: 0,
            operation: op,
            ok: ok,
            code: answer.code,
            message: answer.message,
            url: answer.url,
        ),
    )
    answer
}

/// Whether the value at `key` of a JSON object is true.
fn json_flag(value: json.Value, key: Text) -> Bool {
    match json_field(value, key) {
        .some(held) => match held {
            .bool(flag) => flag,
            .object(fields) => false,
            .array(items) => false,
            .string(text) => false,
            .number(number) => false,
            .null => false,
        },
        .none => false,
    }
}

/// The words a JSON field holds as a list, joined by `between`: empty for one that is no list.
fn json_words(value: json.Value, key: Text, between: Text) -> Text {
    if let held = json_field(value, key) {
        json_arrays(held)
            .map(
                v -> match v {
                    .string(text) => text,
                    .object(_) => "",
                    .array(_) => "",
                    .number(_) => "",
                    .bool(_) => "",
                    .null => "",
                },
            )
            .filter(word -> word != "")
            .join(between)
    } else { "" }
}

/// What an install, a turning on or off, or a removal came to, in the person's words: the
/// engine's kind of failure names what happened, and its own detail is for whoever asks.
fn extension_trouble(kind: Text, said: Text) -> Text {
    if kind == "no-place" {
        std.say("This device keeps no extensions")
    } else if kind == "too-large" {
        std.say("The package is over the size limit")
    } else if kind == "damaged" {
        std.say("The package is damaged, or signed for another extension")
    } else if kind == "broken" {
        std.say("The package holds nothing an extension can use")
    } else if kind == "not-saved" {
        std.say("The package could not be read or saved")
    } else if kind == "package" {
        std.say("That file is no extension package")
    } else if kind == "item" {
        std.say("That is no Chrome Web Store extension")
    } else if kind == "download" {
        std.say("The Chrome Web Store could not be reached")
    } else if kind == "no-package" {
        std.say("The Chrome Web Store has no package for this browser")
    } else if kind == "unknown" {
        std.say("No such extension is installed")
    } else if said != "" {
        said
    } else {
        std.say("The extension could not be changed")
    }
}

/// Whether the page at `url` is one of the Chrome Web Store's own items: such a page offers an
/// install of what it shows, and a look-alike site never does.
fn is_store_page(url: Text) -> Bool {
    let origin = origin_of(url)
    let store = origin == "https://chromewebstore.google.com"
    let legacy = origin == "https://chrome.google.com"
    (store or legacy) and url.contains("/detail/")
}

/// What one of the page's extension buttons is called: the extension's own title for it, else
/// the extension's name, with its badge after it while it shows one.
fn button_label(b: Browser, button: PageButton) -> Text {
    let name = if button.label != "" {
        button.label
    } else if let e = b.extensions.find(x -> x.key == button.ext) {
        e.name
    } else { std.say("Extension") }
    if button.badge != "" { "{name} · {button.badge}" } else { name }
}

/// The notifications the page's extensions showed, as the engine last reported them.
fn notices_of(value: json.Value) -> List<ExtensionNotice> {
    let found: List<ExtensionNotice> = []
    for held in json_arrays(json_field(value, "extension_notices") ?? value) {
        let ext = json_text(held, "id")
        let notification = json_text(held, "notification")
        if ext != "" and notification != "" {
            found = [
                found,
                [
                    ExtensionNotice(
                        ext: ext,
                        notification: notification,
                        title: json_text(held, "title"),
                        message: json_text(held, "message"),
                        buttons: json_words_list(held, "buttons"),
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    found
}

/// The words a JSON field holds as a list, as the list they are.
fn json_words_list(value: json.Value, key: Text) -> List<Text> {
    if let held = json_field(value, key) {
        json_arrays(held)
            .map(
                v -> match v {
                    .string(text) => text,
                    .object(_) => "",
                    .array(_) => "",
                    .number(_) => "",
                    .bool(_) => "",
                    .null => "",
                },
            )
            .filter(word -> word != "")
    } else { [] }
}

/// The toolbar buttons the page's extensions show, as the engine last reported them.
fn buttons_of(value: json.Value) -> List<PageButton> {
    let found: List<PageButton> = []
    for held in json_arrays(json_field(value, "extension_buttons") ?? value) {
        let ext = json_text(held, "id")
        if ext != "" {
            found = [
                found,
                [
                    PageButton(
                        ext: ext,
                        label: json_text(held, "title"),
                        badge: json_text(held, "badge"),
                        enabled: json_flag(held, "enabled"),
                        popup: json_flag(held, "popup"),
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    found
}

/// The extensions an engine's inventory listed.
fn extensions_of(installed: Text) -> List<Extension> {
    let value = match json.parse(installed) {
        .ok(value) => value,
        .err(_) => {
            return []
        }
    }
    let found: List<Extension> = []
    for held in json_arrays(value) {
        let key = json_text(held, "id")
        if key != "" {
            found = [
                found,
                [
                    Extension(
                        key: key,
                        name: json_text(held, "name"),
                        version: json_text(held, "version"),
                        manifest_version: json_number(held, "manifest_version").to_int(),
                        description: json_text(held, "description"),
                        permissions: json_words(held, "permissions", ", "),
                        enabled: json_flag(held, "enabled"),
                        keyword: json_text(held, "keyword"),
                        side_panel: json_flag(held, "side_panel"),
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    found
}

/// The extension whose keyword the address starts with, and what follows it: typing an
/// extension's keyword and a space makes the rest the extension's, as in Chrome. A keyword
/// matches whatever its case.
fn addressed_to(b: Browser, typed: Text) -> Option<Addressed> {
    let trimmed = typed.trim()
    let space = trimmed.find(" ") ?? -1
    if space <= 0 {
        return .none
    }
    let keyword = trimmed.slice(0, space).lower()
    let rest = trimmed.slice(space, trimmed.len()).trim()
    for e in b.extensions {
        if e.enabled and e.keyword != "" and e.keyword.lower() == keyword {
            return .some(Addressed(ext: e.key, name: e.name, text: rest))
        }
    }
    .none
}

/// The rows the extension's suggestions listed, as its window encoded them.
/// The website accounts a window's note lists, each by its id, service and name.
fn website_accounts_of(accounts: Text) -> List<WebsiteAccount> {
    let value = match json.parse(accounts) {
        .ok(value) => value,
        .err(_) => {
            return []
        }
    }
    let found: List<WebsiteAccount> = []
    for held in json_arrays(value) {
        found = [
            found,
            [
                WebsiteAccount(
                    account: json_text(held, "account"),
                    service: json_text(held, "service"),
                    title: json_text(held, "title"),
                ),
            ],
        ]
            .flat_map(x -> x)
    }
    found
}

fn omnibox_rows(rows: Text) -> List<OmniboxRow> {
    let value = match json.parse(rows) {
        .ok(value) => value,
        .err(_) => {
            return []
        }
    }
    let found: List<OmniboxRow> = []
    for held in json_arrays(value) {
        found = [
            found,
            [OmniboxRow(title: json_text(held, "title"), content: json_text(held, "content"))],
        ]
            .flat_map(x -> x)
    }
    found
}

/// What an extension is called, as the Browser's inventory lists it: an extension's own page is
/// named by its identifier alone.
fn extension_named(b: Browser, id: Text) -> Text {
    if let e = b.extensions.find(x -> x.key == id) { e.name } else { std.say("Extension") }
}

/// The site a sign-in page asks the person to sign in on, as its address names it: they type a
/// password there, so they see whose page it is.
fn panel_site(url: Text) -> Text {
    let origin = origin_of(url)
    if origin == "" {
        return url
    }
    origin.split("://").last() ?? origin
}

/// What the window's second page is called: the site a sign-in or a page a script opened
/// shows, or the extension whose panel it is.
fn panel_name(b: Browser, panel: Panel) -> Text {
    if panel.kind == "sign-in" or panel.kind == "popup" {
        panel_site(panel.url)
    } else { extension_named(b, panel.ext) }
}

/// The site the page last reported, when it reported one.
fn site_of(value: json.Value) -> Option<SiteInfo> {
    match json_field(value, "site") {
        .some(held) => {
            let origin = json_text(held, "origin")
            if origin == "" {
                return .none
            }
            .some(
                SiteInfo(
                    origin: origin,
                    generation: json_number(held, "generation").to_int(),
                    blocked: json_number(held, "blocked").to_int(),
                    protected: json_flag(held, "protected"),
                    security: json_text(held, "security"),
                    certificate: json_number(held, "certificate").to_int(),
                    issuer: json_text(held, "issuer"),
                    subject: json_text(held, "subject"),
                    permissions: permissions_of(held),
                    can_reset: json_flag(held, "can_reset"),
                ),
            )
        }
        .none => .none,
    }
}

/// What a site's connection is, as the engine names it, in the person's words.
fn security_words(security: Text) -> Text {
    if security == "secure" {
        std.say("Secure connection")
    } else if security == "insecure" {
        std.say("Not encrypted")
    } else if security == "unverified" {
        std.say("Unverified certificate")
    } else if security == "mixed" {
        std.say("Some of the page is not encrypted")
    } else { std.say("Unknown connection") }
}

/// What kind of login form the engine found, in the person's words.
fn login_kind_words(kind: Text) -> Text {
    if kind == "current-password" or kind == "sign-in" or kind == "login" {
        std.say("Sign-in form")
    } else if kind == "new-password" or kind == "create" or kind == "sign-up" {
        std.say("New password form")
    } else { std.say("Form") }
}

/// What a site may ask for, as the engine names it, in the person's words.
fn permission_kind_words(kind: Text) -> Text {
    if kind == "camera" {
        std.say("Camera")
    } else if kind == "microphone" {
        std.say("Microphone")
    } else if kind == "location" {
        std.say("Location")
    } else if kind == "notifications" {
        std.say("Notifications")
    } else { std.say("Something else") }
}

/// What one of a site's permissions stands at, as the engine names it, in the person's words.
fn permission_state_words(state: Text) -> Text {
    if state == "allowed" {
        std.say("allowed")
    } else if state == "blocked" {
        std.say("blocked")
    } else if state == "ask" { std.say("asks first") } else { std.say("set") }
}

/// One permission the site reported, `camera: ask` as the engine names it, in the person's words.
fn permission_line(permission: Text) -> Text {
    let parts = permission.split(": ")
    let kind = parts.at(0) ?? ""
    let state = parts.at(1) ?? ""
    if kind == "" {
        std.say("A permission of this site")
    } else if state == "" {
        permission_kind_words(kind)
    } else {
        let what = permission_kind_words(kind)
        let standing = permission_state_words(state)
        std.say("{what}: {standing}")
    }
}

/// Each permission the site reported, as what it is for and what it may do.
fn permissions_of(site: json.Value) -> List<Text> {
    let found: List<Text> = []
    for held in json_arrays(json_field(site, "permissions") ?? site) {
        let kind = json_text(held, "kind")
        if kind != "" {
            found = [found, ["{kind}: {json_text(held, "state")}"]].flat_map(x -> x)
        }
    }
    found
}

/// One direction's pages the page last reported, as its window reads them.
fn history_of(value: json.Value, direction: Text) -> List<HistoryEntry> {
    let found: List<HistoryEntry> = []
    for held in json_arrays(json_field(value, direction) ?? value) {
        let entry = json_number(held, "id").to_int()
        if entry != 0 {
            let url = json_text(held, "url")
            let title = json_text(held, "title")
            found = [
                found,
                [
                    HistoryEntry(
                        entry: entry,
                        url: url,
                        title: if title == "" { url } else { title },
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    found
}

/// What the page answered an answer to what it waited on: whether it took it. It changes nothing
/// of the page's record, so the person's approve of it is the whole of what it does.
fn page_answered(raw: Text) -> Answer {
    match json.parse(raw) {
        .ok(value) => Answer(ok: json_flag(value, "ok"), message: json_text(value, "message")),
        .err(_) => Answer(message: raw),
    }
}

/// The question the page asks, when it asks one.
fn dialog_of(value: json.Value) -> Option<DialogAsk> {
    match json_field(value, "dialog") {
        .some(held) => .some(
            DialogAsk(
                request: json_number(held, "id").to_int(),
                kind: json_text(held, "kind"),
                origin: json_text(held, "origin"),
                message: json_text(held, "message"),
                default_text: json_text(held, "default_text"),
            ),
        ),
        .none => .none,
    }
}

/// The video the page shows, when one shows.
fn media_of(value: json.Value) -> Option<PageMedia> {
    match json_field(value, "media") {
        .some(held) => .some(
            PageMedia(element: json_text(held, "element"), label: json_text(held, "label")),
        ),
        .none => .none,
    }
}

/// The menu the page showed, when one shows.
fn menu_of(value: json.Value) -> Option<MenuAsk> {
    match json_field(value, "menu") {
        .some(held) => {
            let rows = match json_field(held, "items") {
                .some(items) => json_arrays(items),
                .none => [],
            }
            return .some(
                MenuAsk(
                    request: json_number(held, "id").to_int(),
                    link: json_text(held, "link"),
                    items: menu_rows(rows, ""),
                ),
            )
        }
        .none => .none,
    }
}

/// The rows of the page's menu, a submenu's rows named after the menu they stand under.
fn menu_rows(items: List<json.Value>, under: Text) -> List<MenuRow> {
    let rows: List<MenuRow> = []
    for item in items {
        let label = json_text(item, "label")
        let named = if under == "" { label } else { "{under} › {label}" }
        if json_flag(item, "separator") {
            rows = [rows, [MenuRow(separator: true)]].flat_map(x -> x)
        } else if let nested = json_field(item, "items") {
            rows = [rows, menu_rows(json_arrays(nested), named)].flat_map(x -> x)
        } else {
            rows = [
                rows,
                [
                    MenuRow(
                        command: json_number(item, "command").to_int(),
                        label: named,
                        disabled: not json_flag(item, "enabled"),
                        checked: json_flag(item, "checked"),
                    ),
                ],
            ]
                .flat_map(x -> x)
        }
    }
    rows
}

/// The moment at Unix seconds `seconds`.
fn epoch(seconds: Float) -> Time {
    Time("1970-01-01T00:00:00Z") + Duration(seconds.to_int())
}

/// The browser's window: the page it shows, or, in a workspace that has none, an empty window
/// that offers one.
view page_window(b: Browser) {
    let first = b.pages.find(p -> true)
    if let p = first {
        page_view(p)
    } else {
        ui.NavShell(content_kind: .document, title: "Browser") {
            content: {
                ui.EmptyState(
                    "No page is open",
                    message: "Open a page to browse: its address, or what to search for.",
                    icon: .globe,
                ) {
                    on act: new_page(b)
                }
            }
        }
    }
}

/// The page's menu: its library and settings, what this site's protection and forms are, and
/// how the page is read.
view page_menu(b: Browser, p: Page, level: State<Float>, reading: State<Bool>) {
    ui.MenuButton("Page", icon: .menu) {
        ui.MenuItem("New page", icon: .plus) {
            on choose: new_page(b)
        }
        ui.MenuItem("New private window", icon: .eye_off) {
            on choose: new_private_page(b)
        }
        for a in b.accounts {
            ui.MenuItem(std.say("Open {a.title}"), icon: .user) {
                on choose: open_account(b, a)
            }
        }
        ui.MenuItem("History", icon: .clock) {
            on choose: open(b, view: history)
        }
        ui.MenuItem("Bookmarks", icon: .star) {
            on choose: open(b, view: bookmarks)
        }
        ui.MenuItem("Print", icon: .printer, key: "Ctrl+P") {
            on choose: print_page(p)
        }
        ui.MenuItem("Browser settings", icon: .settings) {
            on choose: open(b, view: browser_settings)
        }
        ui.MenuItem("Reader view", icon: .book_open) {
            on choose: set(reading, not reading).then(reader(p, not reading))
        }
        ui.MenuItem(
            if p.muted { "Unmute the page" } else { "Mute the page" },
            icon: .headphone_off,
        ) {
            on choose: mute(p, not p.muted)
        }
        if let video = p.media {
            ui.MenuItem(
                if p.pip { "Return from picture in picture" } else { "Picture in picture" },
                icon: .picture_in_picture_2,
                description: if video.label == "" { .none } else { .some(video.label) },
            ) {
                on choose: float_page_video(p, not p.pip)
            }
        }
        ui.MenuItem("Earlier pages", icon: .arrow_left) {
            on choose: read_back_history(p)
        }
        ui.MenuItem("Later pages", icon: .arrow_right) {
            on choose: read_forward_history(p)
        }
        for e in p.back {
            ui.MenuItem(e.title, icon: .arrow_left) {
                on choose: go_history_entry(p, e.entry)
            }
        }
        for e in p.forward {
            ui.MenuItem(e.title, icon: .arrow_right) {
                on choose: go_history_entry(p, e.entry)
            }
        }
        ui.MenuItem("Zoom in", icon: .plus) {
            on choose: zoom(p, zoomed_in(level)).then(set(level, zoomed_in(level)))
        }
        ui.MenuItem("Zoom out", icon: .minus) {
            on choose: zoom(p, zoomed_out(level)).then(set(level, zoomed_out(level)))
        }
        let percent = (level * 100.0).round()
        ui.MenuItem(
            if percent == 100 { "Actual size" } else { "Actual size ({percent}%)" },
            icon: .zoom_in,
        ) {
            on choose: zoom(p, 1.0).then(set(level, 1.0))
        }
        ui.MenuItem("Site information", icon: .shield) {
            on choose: read_site(p)
        }
        for e in b.extensions {
            if e.enabled and e.side_panel {
                ui.MenuItem("Open {e.name}’s side panel", icon: .panel_right) {
                    on choose: open_side_panel(p, e)
                }
            }
        }
        ui.MenuItem("Developer tools", icon: .code) {
            on choose: developer_tools(p)
        }
        if origin_of(p.url) != "" {
            let site = origin_of(p.url)
            if b.sites_without_protection.contains(site) {
                ui.MenuItem("Turn tracker protection on for this site", icon: .shield) {
                    on choose: set_site_protection(b, site, true).then(sync_site_exceptions(p))
                }
            } else {
                ui.MenuItem("Turn tracker protection off for this site", icon: .shield) {
                    on choose: set_site_protection(b, site, false).then(sync_site_exceptions(p))
                }
            }
            ui.MenuItem("Forget this site’s forms", icon: .trash) {
                on choose: forget_site_forms(b, site)
            }
        }
    }
}

/// A page's own window: the page itself, with its address and navigation, whether its history
/// is paused, and what was done in it.
view page_view(p: Page) {
    state address: Text = ""
    // What the address bar's suggestions answer: what was typed, once the typing paused.
    state settled: Text = ""
    state failed: Text = ""
    state zoom: Float = p.zoom_level
    state reading: Bool = false
    state answer_text: Text = ""
    state store_link: Text = ""
    let b = root Browser
    if p.fullscreen {
        page_presented(p, failed)
    } else {
        ui.NavShell(content_kind: .document, title: if p.title == "" { p.url } else { p.title }) {
            // The window's search is the page's find: the words are found in the page as they are
            // typed, and cleared, the marks go with them.
            on search(typed: Text): find_in_page(p, typed)
            actions: {
                std.header {
                    ui.Button("Back", icon: .arrow_left, role: .plain) {
                        on press: back(p).then(refresh_page(p))
                    }
                    ui.Button("Forward", icon: .arrow_right, role: .plain) {
                        on press: forward(p).then(refresh_page(p))
                    }
                    ui.Button("Reload", icon: .refresh, role: .plain) {
                        on press: reload(p).then(refresh_page(p))
                    }
                    if let video = p.media {
                        ui.Button(
                            if p.pip {
                                "Return from picture in picture"
                            } else { "Picture in picture" },
                            icon: .picture_in_picture_2,
                            role: .plain,
                            toggled: p.pip,
                            description: if video.label == "" {
                                .none
                            } else { .some(video.label) },
                        ) {
                            on press: float_page_video(p, not p.pip)
                        }
                    }
                    ui.Button("Pause history", icon: .clock, toggled: p.paused) {
                        on press: toggle_pause(p)
                    }
                }
            }
            content: {
                stack {
                    if not p.shared {
                        ui.Banner(
                            "Private window",
                            message: "This window keeps nothing when it closes: no history, no sign-ins, and no cookies but the page's own.",
                            kind: .info,
                        )
                    }
                    if p.visitor {
                        ui.Banner(
                            "Not signed in",
                            message: "An agent browses this page as any visitor would: none of your sign-ins, cookies or saved logins reach it.",
                            kind: .info,
                        )
                    }
                    if p.account_notice != "" {
                        ui.Banner(p.account_notice, kind: .info)
                    }
                    stack(padding: .related) {
                        ui.TextInput(
                            bind: address,
                            placeholder: if p.url == "" { "Address and search" } else { p.url },
                            key: "l",
                        ) {
                            on changed(typed: Text): typed_address(p, typed)
                            on blur: address_left(p)
                            on submit(typed: Text): submit_address(p, typed).then(set(address, ""))
                        }
                        // What the bar offers follows the typing once it pauses, not every key.
                        if settled != address {
                            ui.Timer(after: 0.15, restart: address) {
                                on tick: set(settled, address)
                            }
                        }
                        ui.Button("Bookmark", icon: .star, role: .plain) {
                            on press: bookmark(b, p.url, .some(p.title))
                        }
                        if is_store_page(p.url) {
                            ui.Button("Add to Browser", icon: .puzzle, role: .plain) {
                                on press: install_store_extension(p, p.url).then(list_extensions(p))
                            }
                        }
                        ui.Button("Questions", icon: .triangle_alert, role: .plain) {
                            on press: refresh_page(p)
                        }
                        page_menu(b, p, zoom, reading)
                    }
                    if let om = p.omnibox {
                        stack(padding: .related) {
                            ui.Section(if om.name == "" { "From the address" } else { om.name }) {
                                for r in om.rows {
                                    omnibox_row(p, r)
                                }
                            }
                        }
                    }
                    if not p.buttons.is_empty() {
                        row {
                            for button in p.buttons {
                                ui.Button(button_label(b, button), icon: .puzzle, role: .plain) {
                                    on press: click_extension(p, button)
                                }
                            }
                        }
                    }
                    for n in p.notices {
                        stack(padding: .related) {
                            ui.Banner(
                                if n.title == "" { "Notification" } else { n.title },
                                message: n.message,
                                kind: .info,
                            )
                            row {
                                for offered in n.buttons {
                                    ui.Button(offered, role: .plain) {
                                        on press: choose_extension_notice(
                                            p,
                                            n,
                                            "{n.buttons.index_of(offered) ?? 0}",
                                        )
                                    }
                                }
                                ui.Button("Dismiss", icon: .x, role: .plain) {
                                    on press: dismiss_extension_notice(p, n)
                                }
                            }
                        }
                    }
                    if address.trim() != "" {
                        let found = suggestions(b, settled)
                        if not found.is_empty() {
                            stack(padding: .related) {
                                ui.Section("Suggestions") {
                                    // Choosing one opens what it names, and the bar empties.
                                    for s in found {
                                        ui.ListRow(
                                            title: s.title,
                                            secondary: s.secondary,
                                            icon: if s.kind == .bookmark {
                                                .star
                                            } else if s.kind == .search {
                                                .search
                                            } else { .clock },
                                        ) {
                                            on activate: navigate(p, s.url).then(set(address, ""))
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if p.find_count > 0 {
                        row(gap: .related, align: .center, padding: .related) {
                            text("{p.find_at} of {p.find_count}", role: .caption, secondary: true)
                            ui.Button(
                                "Previous match",
                                icon: .arrow_up,
                                role: .plain,
                                icon_only: true,
                            ) {
                                on press: find_in_page(p, p.find_text, forward: false, next: true)
                            }
                            ui.Button(
                                "Next match",
                                icon: .arrow_down,
                                role: .plain,
                                icon_only: true,
                            ) {
                                on press: find_in_page(p, p.find_text, next: true)
                            }
                            ui.Button("Clear the find", icon: .x, role: .plain, icon_only: true) {
                                on press: find_in_page(p, "")
                            }
                        }
                    }
                    if p.loading and p.progress > 0.0 and p.progress < 1.0 {
                        ui.ProgressBar(p.progress)
                    }
                    if failed != "" {
                        ui.EmptyState(
                            "This page could not be shown",
                            message: failed,
                            icon: .globe,
                            error: true,
                        ) {
                            on act: set(failed, "")
                        }
                    } else {
                        row(gap: .none, align: .stretch) {
                            layers(fill: 1.0) {
                                ui.WebView(
                                    title: if p.title == "" { p.url } else { p.title },
                                    uri: p.url,
                                    pip: p.pip,
                                ) {
                                    on navigate(address: Text): went_to(p, address)
                                        .then(
                                            refresh_page(p),
                                        )
                                    on fail(why: Text): set(failed, why)
                                    on picture_in_picture_closed: float_page_video(p, false)
                                }
                                if let menu = p.menu {
                                    ui.ContextMenu {
                                        items: {
                                            for row in menu.items {
                                                if row.separator {
                                                    ui.MenuSeparator {
                                                    }
                                                } else {
                                                    ui.MenuItem(
                                                        row.label,
                                                        checked: .some(row.checked),
                                                        disabled: row.disabled,
                                                    ) {
                                                        on choose: context_menu_action(
                                                                p,
                                                                menu.request,
                                                                row.command,
                                                            )
                                                            .then(
                                                                refresh_page(p),
                                                            )
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if let panel = p.panel {
                                stack(min: 18rem, max: 30rem, fill: 0.0, gap: .tight) {
                                    row(gap: .related, align: .center) {
                                        text(
                                            panel_name(b, panel),
                                            role: .label,
                                            truncate: true,
                                            fill: 1.0,
                                        )
                                        ui.Button("Close", icon: .x, role: .plain) {
                                            on press: close_panel(p)
                                        }
                                    }
                                    layers(fill: 1.0) {
                                        ui.WebView(title: panel_name(b, panel), surface: "panel") {
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let ask = p.dialog {
                        ui.ModalDialog(
                            title: "This page asks",
                            description: .some(ask.origin),
                            open: true,
                            primary: if ask.kind == "prompt" { "Send" } else { "Close" },
                            secondary: if ask.kind == "alert" { .none } else { .some("Cancel") },
                        ) {
                            body: {
                                ui.Text(ask.message)
                                if ask.kind == "prompt" {
                                    ui.TextInput(bind: answer_text, autofocus: true) {
                                    }
                                }
                            }
                            on act_primary: answer_page_dialog(
                                    p,
                                    ask.request,
                                    true,
                                    .some(if ask.kind == "prompt" { answer_text } else { "" }),
                                )
                                .then(refresh_page(p))
                            on dismiss: answer_page_dialog(p, ask.request, false)
                                .then(refresh_page(p))
                        }
                    }
                    if p.login_notice != "" {
                        ui.Banner("Logins", message: p.login_notice, kind: .info)
                    }
                    if p.print_notice != "" {
                        ui.Banner("Printing", message: p.print_notice, kind: .warning)
                    }
                    stack(padding: .related) {
                        ui.Button("Find this page’s logins", icon: .key, role: .plain) {
                            on press: discover_logins(p)
                        }
                        ui.Button("Saved logins", icon: .key, role: .plain) {
                            on press: list_saved_logins(p)
                        }
                        if p.capture != "" {
                            ui.Button("Save the login", icon: .check, role: .plain) {
                                on press: save_login(p)
                            }
                        }
                    }
                    if not p.login_forms.is_empty() {
                        stack(padding: .related) {
                            ui.Section("Logins on this page") {
                                for f in p.login_forms {
                                    stack(padding: .related) {
                                        ui.ListRow(
                                            title: if f.username == "" {
                                                login_kind_words(f.kind)
                                            } else { f.username },
                                            secondary: login_kind_words(f.kind),
                                            mark: .key,
                                        )
                                        ui.Button("Save this login", icon: .key, role: .plain) {
                                            on press: capture_login(p, f)
                                        }
                                        if let a = p.login_accounts.find(
                                                x -> f.username != "" and x.username == f.username,
                                            ) {
                                            ui.Button(
                                                "Update the saved login",
                                                icon: .refresh,
                                                role: .plain,
                                            ) {
                                                on press: update_saved_login(p, f, a)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if not p.login_accounts.is_empty() {
                        stack(padding: .related) {
                            ui.Section("Saved logins") {
                                for a in p.login_accounts {
                                    stack(padding: .related) {
                                        ui.ListRow(
                                            title: a.username,
                                            secondary: if a.own {
                                                "Saved by Browser in your vault"
                                            } else { "In your vault: fills once you allow it" },
                                            mark: .key,
                                        )
                                        if let f = p.login_forms.find(x -> true) {
                                            ui.Button("Fill", icon: .key, role: .plain) {
                                                on press: fill_saved_login(p, f, a)
                                            }
                                        }
                                        ui.Button("Delete", icon: .trash, role: .plain) {
                                            on press: delete_saved_login(p, a)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    stack(padding: .related) {
                        ui.Section("Extensions") {
                            if b.extension_notice != "" {
                                ui.Banner("Extensions", message: b.extension_notice, kind: .info)
                            }
                            ui.Button(
                                "Read the installed extensions",
                                icon: .puzzle,
                                role: .plain,
                            ) {
                                on press: list_extensions(p)
                            }
                            for e in b.extensions {
                                stack(padding: .related) {
                                    ui.ListRow(
                                        title: e.name,
                                        secondary: if e.enabled {
                                            "On ({e.version})"
                                        } else { "Off ({e.version})" },
                                        mark: .puzzle,
                                    )
                                    if e.description != "" {
                                        ui.Text(e.description)
                                    }
                                    if e.permissions != "" {
                                        ui.Text("May use {e.permissions}")
                                    }
                                    ui.Button(
                                        if e.enabled {
                                            "Turn the extension off"
                                        } else { "Turn the extension on" },
                                        icon: .check,
                                        role: .plain,
                                    ) {
                                        on press: enable_extension(p, e).then(list_extensions(p))
                                    }
                                    ui.Button("Remove", icon: .trash, role: .plain) {
                                        on press: remove_extension(p, e).then(list_extensions(p))
                                    }
                                }
                            }
                            stack(padding: .related) {
                                ui.Button("Install from a file", icon: .file_plus, role: .plain) {
                                    on press: file
                                        .choose(
                                            install_extension(p),
                                        )
                                        .then(
                                            list_extensions(p),
                                        )
                                }
                                ui.TextInput(
                                    bind: store_link,
                                    placeholder: "Link to a Chrome Web Store extension",
                                    key: "e",
                                )
                                ui.Button("Add from the store", icon: .puzzle, role: .plain) {
                                    on press: install_store_extension(p, store_link)
                                        .then(
                                            list_extensions(p),
                                        )
                                }
                                ui.Button("Check for updates", icon: .refresh, role: .plain) {
                                    on press: update_extensions(p).then(list_extensions(p))
                                }
                            }
                        }
                    }
                    if let site = p.site {
                        stack(padding: .related) {
                            ui.Section("Site information") {
                                ui.ListRow(
                                    title: site.origin,
                                    secondary: security_words(site.security),
                                    mark: .shield,
                                )
                                ui.ListRow(
                                    title: "Requests blocked",
                                    secondary: "{site.blocked}",
                                    mark: .shield,
                                )
                                if site.issuer != "" {
                                    ui.ListRow(
                                        title: "Certificate",
                                        secondary: site.issuer,
                                        mark: .lock,
                                    )
                                }
                                for permission in site.permissions {
                                    ui.Text(permission_line(permission))
                                }
                                stack(padding: .related) {
                                    ui.Button("Clear this site's data", icon: .trash) {
                                        on press: clear_site_data(p)
                                    }
                                    ui.Button("Block what this site asks for", icon: .shield) {
                                        on press: block_site_permissions(p)
                                    }
                                    if site.can_reset {
                                        ui.Button("Let the site ask again", icon: .refresh) {
                                            on press: ask_site_permissions_again(p)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if not b.downloads.is_empty() {
                        stack(padding: .group) {
                            ui.Section("Downloads") {
                                for d in b.downloads {
                                    download_row(p, d)
                                }
                                ui.Button("Cancel every download", icon: .x, role: .plain) {
                                    on press: cancel_all_downloads()
                                }
                            }
                        }
                    }
                    if not p.steps.is_empty() {
                        stack(padding: .group) {
                            ui.Section("What was done") {
                                for s in p.steps {
                                    step_row(s)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The page while it fills the screen: the page alone, with the way out over its leading top
/// corner. A page that fails to load leaves fullscreen, so its window says why.
view page_presented(p: Page, failed: State<Text>) {
    ui.Fullscreen(showing: true) {
        ui.WebView(title: if p.title == "" { p.url } else { p.title }, uri: p.url) {
            on navigate(address: Text): went_to(p, address).then(refresh_page(p))
            on fail(why: Text): set(failed, why).then(leave_fullscreen(p))
        }
        on exit: leave_fullscreen(p)
    }
}

/// One row the extension whose keyword the address addresses offers: what choosing it sends the
/// extension.
view omnibox_row(p: Page, r: OmniboxRow) {
    ui.ListRow(title: r.title, secondary: r.content, icon: .puzzle) {
        on activate: choose_omnibox(p, r.content)
    }
}

/// What the address bar offers for `typed`, as the person's own browsers answer it: the pages
/// they saved, then the ones they open most often and most lately (this browser's history, as
/// the system ranks it by their use, then the pages another browser's history brought over,
/// newest first), each page once, and the search engine's row. Pages whose address begins with
/// what was typed come before the ones that only mention it. The search leads, unless what was
/// typed is an address or the start of one the person has been to.
fn suggestions(b: Browser, typed: Text) -> List<Suggestion> {
    let words = typed.trim()
    let q = bare(words)
    if q == "" {
        return []
    }
    let saved = b.bookmarks.filter(k -> answers(k.url, k.title, q))
    let visited = ranked(b.visits.filter(v -> answers(v.url, v.title, q)))
    let imported = b.imported_visits
        .filter(v -> answers(v.url, v.title, q))
        .sort_by(v -> v.last, desc)
    let all = saved.map(k -> page_suggestion(.bookmark, k.title, k.url))
        + visited.map(v -> page_suggestion(.visited, v.title, v.url))
        + imported.map(v -> page_suggestion(.imported, v.title, v.url))
    let once: List<Suggestion> = []
    for s in all {
        if not once.any(held -> bare(held.url) == bare(s.url)) {
            once = once + [s]
        }
    }
    let leading = once.filter(s -> bare(s.url).starts_with(q))
    let pages = (leading + once.filter(s -> not bare(s.url).starts_with(q))).take(4)
    let search = Suggestion(
        kind: .search,
        title: std.say("Search for “{words}”"),
        url: search_address(b, words),
        secondary: shown_url(origin_of(engine(b))),
    )
    if looks_like_site(words.lower()) or not leading.is_empty() {
        pages + [search]
    } else { [search] + pages }
}

/// A page the address bar offers, called by its title, or by its address when it has none.
fn page_suggestion(kind: SuggestionKind, title: Text, url: Text) -> Suggestion {
    let shown = shown_url(url)
    Suggestion(
        kind: kind,
        title: if title.trim() == "" or title == url { shown } else { title },
        url: url,
        secondary: shown,
    )
}

/// Whether a page answers the words `q` (lowercase, as [`bare`] leaves them): its address or
/// its title holds each of them.
fn answers(url: Text, title: Text, q: Text) -> Bool {
    let key = bare(url)
    let named = title.lower()
    q.split(" ").filter(w -> w != "").all(w -> key.contains(w) or named.contains(w))
}

/// An address or what was typed, lowercase, without its scheme or `www.`: what the address bar
/// compares.
fn bare(url: Text) -> Text {
    let lower = url.trim().lower()
    let rest = match lower.find("://") {
        .some(at) => lower.slice(at + 3, lower.len()),
        .none => lower,
    }
    if rest.starts_with("www.") { rest.slice(4, rest.len()) } else { rest }
}

/// An address as the address bar shows it: without its scheme, `www.` or a closing slash.
fn shown_url(url: Text) -> Text {
    let said = url.trim()
    let rest = match said.find("://") {
        .some(at) => said.slice(at + 3, said.len()),
        .none => said,
    }
    let host = if rest.lower().starts_with("www.") { rest.slice(4, rest.len()) } else { rest }
    if host.ends_with("/") { host.slice(0, host.len() - 1) } else { host }
}

/// One step of what was done in a page: what it was, what it acted on and how it ended.
view step_row(s: PageStep) {
    ui.ListRow(
        title: s.operation,
        secondary: step_secondary(s),
        mark: if s.ok { .check } else { .triangle_alert },
    )
}

/// A bookmark's own window: where it is, its name and folder, and the way to take it back.
view bookmark_page(k: Bookmark) {
    std.page(k) {
        content: {
            ui.PropertyList {
                ui.Property("Address", k.url, copyable: true)
            }
            ui.Section("Details") {
                ui.ControlRow(label: "Name") {
                    ui.TextInput(bind: k.title, fill: 1.0)
                }
                ui.ControlRow(label: "Folder") {
                    ui.TextInput(bind: k.folder, placeholder: "Top of the bookmarks", fill: 1.0)
                }
            }
            ui.Button("Remove bookmark", icon: .trash) {
                on press: remove_bookmark(k)
            }
        }
    }
}

/// An address the person fills forms with: what it is called, and the answer to each question a
/// form asks. Every field answers the question of the same name.
view address_page(a: Address) {
    std.page(a) {
        content: {
            ui.Section("The address") {
                ui.ControlRow(label: "Name for it") {
                    ui.TextInput(bind: a.label, placeholder: "Home", fill: 1.0)
                }
                ui.ControlRow(label: "Full name") {
                    ui.TextInput(bind: a.full_name, fill: 1.0)
                }
                ui.ControlRow(label: "Given name") {
                    ui.TextInput(bind: a.given_name, fill: 1.0)
                }
                ui.ControlRow(label: "Family name") {
                    ui.TextInput(bind: a.family_name, fill: 1.0)
                }
                ui.ControlRow(label: "Street") {
                    ui.TextInput(bind: a.street, fill: 1.0)
                }
                ui.ControlRow(label: "City") {
                    ui.TextInput(bind: a.city, fill: 1.0)
                }
                ui.ControlRow(label: "Region") {
                    ui.TextInput(bind: a.region, fill: 1.0)
                }
                ui.ControlRow(label: "Postal code") {
                    ui.TextInput(bind: a.postal_code, fill: 1.0)
                }
                ui.ControlRow(label: "Country") {
                    ui.TextInput(bind: a.country, fill: 1.0)
                }
            }
            ui.Button("Delete address", icon: .trash) {
                on press: delete_address(a)
            }
        }
    }
}

/// A visit's own window: where it is, when it was last opened and how often.
view visit_page(v: Visit) {
    std.page(v) {
        content: {
            ui.PropertyList {
                ui.Property("Address", v.url, copyable: true)
                ui.Property("Last opened", "{v.last}", copyable: true)
                ui.Property("Opened", "{v.count}")
            }
            ui.Button("Forget this page", icon: .trash) {
                on press: forget_visit(v)
            }
        }
    }
}

/// The library on the pages visited.
view history(b: Browser) {
    state section: Text = "history"
    library(b, section)
}

/// The library on the pages saved.
view bookmarks(b: Browser) {
    state section: Text = "bookmarks"
    library(b, section)
}

/// The library: the pages visited and the pages saved, the section the person chose open beside
/// them.
view library(b: Browser, section: State<Text>) {
    let title = if section == "bookmarks" { "Bookmarks" } else { "History" }
    state query: Text = ""
    ui.NavShell(content_kind: .list, title: title, sidebar_title: "Library") {
        on search(typed: Text): set(query, typed)
        sidebar: {
            ui.NavList(selected: section) {
                ui.NavItem("history", "History", icon: .clock) {
                    on select: set(section, "history")
                }
                ui.NavItem("bookmarks", "Bookmarks", icon: .star) {
                    on select: set(section, "bookmarks")
                }
            }
        }
        content: {
            if section == "bookmarks" {
                bookmarks_list(b, query)
            } else {
                history_list(b, query)
            }
        }
    }
}

/// The pages saved, under the folder each stands in: a library path (`Work/Reading`) groups
/// what the person, or the browser they came from, put there.
view bookmarks_list(b: Browser, query: Text) {
    let found = b.bookmarks.filter(
        k -> query == ""
            or k.title.lower().contains(query.lower())
            or k.url.contains(query.lower()),
    )
    let folders = bookmark_folders(found)
    if found.is_empty() {
        ui.EmptyState("No bookmarks yet", icon: .star)
    } else {
        ui.Scroll {
            stack {
                for folder in folders {
                    ui.Section(if folder == "" { "Top of the bookmarks" } else { folder }) {
                        for k in found.filter(k -> k.folder == folder) {
                            bookmark_row(k)
                        }
                    }
                }
            }
        }
    }
}

/// A page's row in the library: what it was called, where it is and when it was last read.
view visit_row(v: Visit) {
    ui.ListRow(title: v.title, secondary: v.url, mark: .clock)
}

/// A page taken in from another browser, as its row in the library.
view imported_row(v: ImportedVisit) {
    ui.ListRow(title: v.title, secondary: v.url, mark: .cloud_download)
}

/// A bookmark's row in the library: its name over its address.
view bookmark_row(k: Bookmark) {
    ui.ListRow(title: k.title, secondary: k.url, mark: .star)
}

/// The folders the bookmarks stand in, each once and in the order they first stand there, the
/// top of the library first.
fn bookmark_folders(bookmarks: List<Bookmark>) -> List<Text> {
    let folders: List<Text> = []
    for k in bookmarks {
        if not folders.contains(k.folder) {
            folders = [folders, [k.folder]].flat_map(x -> x)
        }
    }
    folders
}

/// An address's row in the settings: what the person calls it over the name it fills. Opening
/// it edits its answers.
view address_row(a: Address) {
    ui.ListRow(
        title: if a.label == "" { a.full_name } else { a.label },
        secondary: a.full_name,
        mark: .user,
    ) {
        on activate: open(a)
    }
}

/// A remembered form: where it was read, what it asks and how often it was read.
view form_row(f: RememberedForm) {
    ui.DisclosureRow(
        title: if f.label == "" { f.origin } else { f.label },
        secondary: "Read {f.seen} times",
    ) {
        for q in f.questions {
            ui.Text(
                if q.required {
                    "{q.label} ({q.answer}, required)"
                } else { "{q.label} ({q.answer})" },
            )
        }
        ui.Button("Forget this form", icon: .trash) {
            on press: forget_form(f)
        }
    }
}

/// The browser's settings: what it blocks and clears, the search engine and the page a window
/// opens at, the addresses it fills forms with, and the forms the sites asked.
view browser_settings(b: Browser) {
    std.page(b) {
        header: {
            ui.Text("Browser settings", role: .title)
        }
        content: {
            if b.trouble != "" {
                ui.Banner("Something could not be taken in", message: b.trouble, kind: .warning)
            }
            ui.Section("Browsing") {
                ui.ControlRow(label: "Block advertisements and trackers") {
                    ui.Checkbox(bind: b.adblock)
                }
                ui.ControlRow(
                    label: "Clear cookies, site storage and remembered forms when the last window closes",
                ) {
                    ui.Checkbox(bind: b.clear_on_exit)
                }
                ui.ControlRow(label: "Search engine") {
                    ui.TextInput(
                        bind: b.search_engine,
                        placeholder: "https://duckduckgo.com/?q=%s",
                        fill: 1.0,
                    )
                }
                ui.ControlRow(label: "Home page") {
                    ui.TextInput(bind: b.homepage, placeholder: "about:blank", fill: 1.0)
                }
            }
            ui.Section("Addresses") {
                ui.ControlRow(label: "Fill forms with a saved address") {
                    ui.Checkbox(bind: b.addresses_enabled)
                }
                for a in b.addresses {
                    address_row(a)
                }
                ui.Button("Add an address", icon: .user_plus) {
                    on press: add_address(b)
                }
            }
            ui.Section("Sites without tracker protection") {
                if b.sites_without_protection.is_empty() {
                    ui.Text("Every site is protected.", secondary: true)
                } else {
                    for site in b.sites_without_protection {
                        stack {
                            ui.ListRow(title: site, mark: .shield)
                            ui.Button("Protect this site again", icon: .shield) {
                                on press: set_site_protection(b, site, true)
                            }
                        }
                    }
                }
            }
            ui.Section("Remembered forms") {
                if b.forms.is_empty() {
                    ui.Text("No site’s forms were read yet.", secondary: true)
                } else {
                    for f in b.forms {
                        form_row(f)
                    }
                    ui.Button("Forget all remembered forms", icon: .trash) {
                        on press: forget_all_forms(b)
                    }
                }
            }
        }
    }
}

/// The library's history section: this browser's visits, and the pages taken in from another
/// browser below them.
view history_list(b: Browser, query: Text) {
    let visits = b.visits.filter(
        v -> query == ""
            or v.title.lower().contains(query.lower())
            or v.url.contains(query.lower()),
    )
    let imported = b.imported_visits.filter(
        v -> query == ""
            or v.title.lower().contains(query.lower())
            or v.url.contains(query.lower()),
    )
    if visits.is_empty() and imported.is_empty() {
        ui.EmptyState("No pages visited yet", icon: .clock)
    } else {
        ui.Scroll {
            stack {
                for v in visits {
                    visit_row(v)
                }
                if not imported.is_empty() {
                    ui.Section("From another browser") {
                        for v in imported {
                            imported_row(v)
                        }
                    }
                }
            }
        }
    }
}

/// One file the Browser downloads: what it is called, how far its job has come, and what the
/// person may do with it now. One an extension started is the page engine's to save, as it
/// tells; the rest are the Browser's own jobs, as each tells the window.
view download_row(p: Page, d: Download) {
    if let seen = d.seen {
        download_standing(p, d, seen)
    } else if let j = engine.job(d.job) {
        download_standing(p, d, standing_of(j))
    } else {
        // A job this device does not record: it ran on another device, or it was never started.
        download_gone(p, d)
    }
}

/// A Browser job's record as a download's row reads it: its state, how many bytes of how many,
/// where it was saved and as what, and why it failed.
fn standing_of(j: host.Job) -> DownloadJob {
    let saved = match json.decode<DownloadSaved>(j.detail) {
        .ok(said) => said,
        .err(_) => DownloadSaved(),
    }
    let state = match j.state {
        .queued => "queued",
        .running => "running",
        .done => "done",
        .failed => "failed",
        .cancelled => "cancelled",
    }
    DownloadJob(
        state: state,
        done: j.done,
        total: j.total,
        path: saved.path,
        name: saved.name,
        error: j.error,
    )
}

/// A download as it stands now: how far it came or how it ended, and what the person may do.
view download_standing(p: Page, d: Download, now: DownloadJob) {
    stack(padding: .related) {
        ui.ListRow(
            title: if now.name == "" { d.name } else { now.name },
            secondary: download_secondary(now),
            mark: if now.state == "failed" { .triangle_alert } else { .download },
        )
        if now.state == "running" and now.total > 0 {
            ui.ProgressBar(value: now.done.to_float() / now.total.to_float(), label: "Downloading")
        }
        if now.state == "running" and d.seen == .none {
            ui.Button("Cancel", icon: .x, role: .plain) {
                on press: cancel_download(d)
            }
        }
        if now.state == "failed" or now.state == "cancelled" {
            ui.Button("Retry", icon: .refresh, role: .plain) {
                on press: retry_download(p, d)
            }
        }
        if now.state == "done" and now.path != "" {
            ui.Button("Open", icon: .external_link, role: .plain) {
                on press: open_download(now.name, now.path)
            }
            ui.Button("Show in Files", icon: .folder, role: .plain) {
                on press: reveal_download(now.path)
            }
        }
    }
}

/// A download whose job this device does not know, which the person can start again here.
view download_gone(p: Page, d: Download) {
    stack(padding: .related) {
        ui.ListRow(title: d.name, secondary: std.say("Not on this device"), mark: .download)
        ui.Button("Retry", icon: .refresh, role: .plain) {
            on press: retry_download(p, d)
        }
    }
}

/// What a download says under its name: how far it has come, or how it ended.
fn download_secondary(now: DownloadJob) -> Text {
    if now.state == "running" {
        let done = size_text(now.done)
        if now.total > 0 {
            let total = size_text(now.total)
            std.say("{done} of {total}")
        } else { std.say("{done} downloaded") }
    } else if now.state == "done" {
        std.say("Saved to {now.path}")
    } else if now.state == "failed" {
        if now.error == "" { std.say("The download failed") } else { now.error }
    } else if now.state == "cancelled" { std.say("Cancelled") } else { std.say("Waiting to start") }
}

/// The next size up: 1.2 times the page's font, kept between a quarter and five times its own.
fn zoomed_in(level: Float) -> Float {
    clamped_zoom(level * 1.2)
}

/// The next size down, by the same steps.
fn zoomed_out(level: Float) -> Float {
    clamped_zoom(level / 1.2)
}

/// `level` kept to the sizes a person can still read a page at.
fn clamped_zoom(level: Float) -> Float {
    if level < 0.25 { 0.25 } else if level > 5.0 { 5.0 } else { level }
}

/// How many bytes come to a person: `812 B`, `4.2 KB`, `2.0 MB`.
fn size_text(bytes: Int) -> Text {
    if bytes < 1000 {
        return "{bytes} B"
    }
    if bytes < 1000000 {
        let tenths = bytes / 100
        return "{tenths / 10}.{tenths % 10} KB"
    }
    if bytes < 1000000000 {
        let tenths = bytes / 100000
        return "{tenths / 10}.{tenths % 10} MB"
    }
    let tenths = bytes / 100000000
    "{tenths / 10}.{tenths % 10} GB"
}

/// What a step says under its action: the element it acted on, or the address it went to.
fn step_secondary(s: PageStep) -> Text {
    if s.element != "" {
        if s.role == "" { s.element } else { "{s.role} {s.element}" }
    } else if s.to_url != "" { s.to_url } else { s.message }
}

/// A first afternoon in the browser, as a person leaves it: a few pages visited, three saved, a
/// site's form read, an address to fill forms with, and a page with what was done in it. The
/// pages' own content is the web's, which the engine shows; nothing here stands in for it.
scenario "a first afternoon" {
    let b = root Browser
    b.homepage = "https://example.org"
    visit(b, "https://example.org", "Example Domain", Time("2026-11-23T09:12:00Z"))
    visit(b, "https://docs.example.org/guide", "A guide", Time("2026-11-23T09:20:00Z"))
    visit(b, "https://news.example.org/today", "Today’s news", Time("2026-11-23T10:05:00Z"))
    visit(b, "https://example.org", "Example Domain", Time("2026-11-23T11:40:00Z"))
    bookmark(b, "https://docs.example.org/guide", .some("A guide"))
    bookmark(b, "https://example.org/recipes", .some("Recipes"))
    bookmark(b, "https://shop.example.org", .none)
    let profile = b.addresses.insert(
        Address(
            label: "Home",
            full_name: "Ada Lovelace",
            given_name: "Ada",
            family_name: "Lovelace",
            street: "12 Analytical Way",
            city: "London",
            region: "London",
            postal_code: "N1 9AB",
            country: "United Kingdom",
        ),
    )
    profile.label = "Home"
    remember_form(
        b,
        "https://shop.example.org",
        "checkout-1a2b",
        "Delivery address",
        "/checkout",
        [
            "name\ttext\tFull name\ttrue\t",
            "street\ttext\tStreet\ttrue\t",
            "city\ttext\tCity\ttrue\t",
            "region\ttext\tRegion\tfalse\t",
            "postcode\ttext\tPostcode\ttrue\t",
            "country\tchoice\tCountry\ttrue\tUnited Kingdom|Ireland|France",
        ],
        true,
    )
    let page = b.pages.insert(Page(url: "https://shop.example.org/checkout", title: "Checkout"))
    page.steps.insert(
        PageStep(
            at: 1200,
            operation: "read-page",
            url: "https://shop.example.org/checkout",
            element: "",
            role: "",
        ),
    )
    page.steps.insert(
        PageStep(
            at: 3400,
            operation: "fill-page",
            url: "https://shop.example.org/checkout",
            element: "Full name",
            role: "textbox",
            text: .some("Ada Lovelace"),
        ),
    )
}
