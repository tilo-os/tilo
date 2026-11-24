app so.tilo.browser "Browser" (
    icon: .globe,
    category: .network,
    tagline: "Web browser with ad blocking and Chrome extensions",
    keywords: ["web", "internet", "surf", "website", "url", "www", "web browser", "chrome", "safari", "edge", "firefox", "browser", "browsen", "webseite", "internetseite"],
)

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
/// reads of it is this.
type Page (title: title) {
    url: Text
    title: Text
    /// History is paused in this page's window: pages opened there are not added to the person's
    /// history.
    paused: Bool = false
    /// What was done in the page, newest last. The newest thousand are kept.
    steps: Collection<PageStep>
    /// What the page waits on, as its window last read it: a question a person answers, and the
    /// menu it showed. Engine state, refreshed when the page's window opens it and after every
    /// answer, never the page's own record.
    dialog: Option<DialogAsk> = .none
    sign_in: Option<SignInAsk> = .none
    permission: Option<PermissionAsk> = .none
    held: Collection<HeldAsk>
    menu: Option<MenuAsk> = .none
    /// How far the page has loaded, what it is zoomed to, and where the find bar stands.
    loading: Bool = false
    progress: Float = 0.0
    zoom_level: Float = 1.0
    find_at: Int = 0
    find_count: Int = 0

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

/// A server or a proxy asking for a user name and a password.
type SignInAsk (title: site) {
    request: Int
    /// Who asks, and where the credentials go.
    asker: Text
    /// The asker as a person reads a site.
    site: Text
    proxy: Bool = false
    realm: Text = ""
    retry: Bool = false
}

/// A site asking to use the device: its camera, its microphone, its location.
type PermissionAsk (title: origin) {
    request: Int
    origin: Text
    /// What it asks for, as the engine names them.
    permissions: Text
}

/// A request the page holds until a person lets it leave.
type HeldAsk (title: origin) {
    request: Int
    /// `submit`, `navigate`, `request`, `download` or `payment`.
    kind: Text
    method: Text = ""
    origin: Text
    has_picture: Bool = false
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
    level: Option<Float> = .none
    values: Option<List<Text>> = .none
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
    /// Forms are filled with a saved address while the person has one and leaves this on.
    addresses_enabled: Bool = false
}

root Browser

default view page_window for Browser

default view page_view for Page

default view bookmark_page for Bookmark

default view visit_page for Visit

/// Opens a page in a window of its own. Every window of the app is one page; what it shows, its
/// address and its own back and forward are the system's.
primary action new_page(b: Browser) -> Page (icon: .plus, label: "New page", key: "t") {
    b.pages.insert(Page(url: b.homepage, title: ""))
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

/// Write a secret into a password or card field: it is never kept in the page's record.
action fill_page_secret(
    p: Page,
    element: Text,
    text: Text,
    frame: Option<Text> = .none,
) -> Answer uses page (icon: .key, label: "Fill a secret") {
    let request = json.encode(
        Ask(op: "fill", element: .some(element), text: .some(text), secret: true, frame: frame),
    )
    ask_page(p, request)
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

/// Open an address in the page's own window.
action navigate(p: Page, url: Text) -> Answer uses page (
    icon: .arrow_right,
    label: "Go to an address",
) {
    let request = json.encode(Ask(op: "navigate", url: .some(url)))
    let answer = ask_page(p, request)
    p.url = url
    answer
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

/// Find words in the page, as the find bar does.
action find_in_page(
    p: Page,
    text: Text,
    forward: Bool = true,
    next: Bool = false,
) -> Answer uses page {
    let request = json.encode(
        Ask(op: "find_in_page", text: .some(text), forward: forward, next: next),
    )
    ask_page(p, request)
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

/// Answer a server's sign-in request, or cancel it with no username.
action answer_page_sign_in(
    p: Page,
    id: Int,
    asker: Text,
    username: Option<Text> = .none,
    password: Option<Text> = .none,
) -> Answer uses page (icon: .key, label: "Sign in to the page") {
    let request = json.encode(
        Ask(
            op: "answer_sign_in",
            id: .some(id),
            asker: .some(asker),
            username: username,
            password: password,
        ),
    )
    ask_page(p, request)
}

/// Let a held request leave, or keep the page as it is.
action answer_held_request(p: Page, id: Int, allow: Bool) -> Answer uses page {
    let request = json.encode(Ask(op: "answer_held_request", id: .some(id), allow: allow))
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
    p.sign_in = sign_in_of(value)
    p.permission = permission_of(value)
    p.menu = menu_of(value)
    p.held.remove_where(x -> true)
    if let rows = json_field(value, "held") {
        for row in json_arrays(rows) {
            p.held.insert(
                HeldAsk(
                    request: json_number(row, "id").to_int(),
                    kind: json_text(row, "kind"),
                    method: json_text(row, "method"),
                    origin: json_text(row, "origin"),
                    has_picture: json_flag(row, "has_picture"),
                ),
            )
        }
    }
    p.loading = json_flag(value, "loading")
    p.progress = json_number(value, "progress")
    p.zoom_level = json_number(value, "zoom")
    let found = json_field(value, "find") ?? value
    p.find_at = json_number(found, "active").to_int()
    p.find_count = json_number(found, "count").to_int()
    Answer(ok: true, message: "read")
}

/// The page's window opened: it reads what the page asks and shows before anyone looks.
opened action opened_page(p: Page) uses page {
    refresh_page(p)
}

/// Answers a site's request to use the device.
action answer_permission(p: Page, id: Int, allow: Bool) -> Answer uses page {
    let request = json.encode(Ask(op: "answer_permission", id: .some(id), allow: allow))
    ask_page(p, request)
}

/// Turns a site's request down without answering it, so it may ask again later.
action dismiss_permission(p: Page, id: Int) -> Answer uses page {
    let request = json.encode(Ask(op: "dismiss_permission", id: .some(id)))
    ask_page(p, request)
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
    p.url = url
    if not p.paused { arrived_at(root Browser, url) }
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
    b.trouble = if taken == 0 { "This history holds no page" } else { "" }
}

/// Imports the bookmarks another browser holds, from the bundle this app's own import hands
/// over: the `ImportBundle` JSON with bookmark chunks (`{"bookmarks": [{"url": "...",
/// "title": "...", "folder": "Work"}]}`). Importing the same file twice keeps one bookmark of
/// each page.
action import_bookmarks(b: Browser, bundle: Text) (icon: .file_plus, label: "Import bookmarks") {
    let found = bookmarks_of_json(bundle.trim())
    if found.is_empty() {
        b.trouble = "This file holds no bookmark"
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
    if lower.starts_with("http://") or lower.starts_with("https://") {
        said
    } else if looks_like_site(lower) {
        "https://{said}"
    } else {
        let engine = if b.search_engine.contains("%s") {
            b.search_engine
        } else { "https://duckduckgo.com/?q=%s" }
        engine.replace("%s", percent(said))
    }
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

/// The bookmarks a JSON bundle holds.
fn bookmarks_of_json(text: Text) -> List<Bookmark> {
    let value = match json.parse(text) {
        .ok(value) => value,
        .err(why) => {
            return []
        }
    }
    let found: List<Bookmark> = []
    for entry in json_arrays(json_field(value, "bookmarks") ?? value) {
        let url = normalized(json_text(entry, "url"))
        if url != "" {
            let title = json_text(entry, "title")
            found = [
                found,
                [
                    Bookmark(
                        url: url,
                        title: if title == "" { url } else { title },
                        folder: json_text(entry, "folder"),
                    ),
                ],
            ]
                .flat_map(x -> x)
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

/// The sign-in the page waits on, when one waits.
fn sign_in_of(value: json.Value) -> Option<SignInAsk> {
    match json_field(value, "sign_in") {
        .some(held) => .some(
            SignInAsk(
                request: json_number(held, "id").to_int(),
                asker: json_text(held, "asker"),
                site: json_text(held, "site"),
                proxy: json_flag(held, "proxy"),
                realm: json_text(held, "realm"),
                retry: json_flag(held, "retry"),
            ),
        ),
        .none => .none,
    }
}

/// The permission a site asks for, when one waits.
fn permission_of(value: json.Value) -> Option<PermissionAsk> {
    match json_field(value, "permission") {
        .some(held) => .some(
            PermissionAsk(
                request: json_number(held, "id").to_int(),
                origin: json_text(held, "origin"),
                permissions: json_text(held, "permissions"),
            ),
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
        ui.MenuItem("History", icon: .clock) {
            on choose: open(b, view: library)
        }
        ui.MenuItem("Bookmarks", icon: .star) {
            on choose: open(b, view: library)
        }
        ui.MenuItem("Browser settings", icon: .settings) {
            on choose: open(b, view: browser_settings)
        }
        ui.MenuItem("Reader view", icon: .book_open) {
            on choose: set(reading, not reading).then(reader(p, not reading))
        }
        ui.MenuItem("Zoom in", icon: .plus) {
            on choose: zoom(p, level * 1.1).then(set(level, level * 1.1))
        }
        ui.MenuItem("Zoom out", icon: .minus) {
            on choose: zoom(p, level / 1.1).then(set(level, level / 1.1))
        }
        ui.MenuItem("Actual size", icon: .zoom_in) {
            on choose: zoom(p, 1.0).then(set(level, 1.0))
        }
        if origin_of(p.url) != "" {
            let site = origin_of(p.url)
            if b.sites_without_protection.contains(site) {
                ui.MenuItem("Turn tracker protection on for this site", icon: .shield) {
                    on choose: set_site_protection(b, site, true)
                }
            } else {
                ui.MenuItem("Turn tracker protection off for this site", icon: .shield) {
                    on choose: set_site_protection(b, site, false)
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
    state failed: Text = ""
    state finding: Bool = false
    state find_text: Text = ""
    state zoom: Float = 1.0
    state reading: Bool = false
    state answer_text: Text = ""
    state sign_user: Text = ""
    state sign_password: Text = ""
    let b = root Browser
    ui.NavShell(content_kind: .document, title: if p.title == "" { p.url } else { p.title }) {
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
                ui.Button("Find in page", icon: .search, role: .plain) {
                    on press: set(finding, not finding)
                }
                ui.Button("Pause history", icon: .clock, toggled: p.paused) {
                    on press: toggle_pause(p)
                }
            }
        }
        content: {
            stack {
                stack(padding: .related) {
                    ui.TextInput(
                        bind: address,
                        placeholder: if p.url == "" { "Address and search" } else { p.url },
                        key: "l",
                    ) {
                        on submit(typed: Text): navigate(p, typed)
                    }
                    ui.Button("Bookmark", icon: .star, role: .plain) {
                        on press: bookmark(b, p.url, .some(p.title))
                    }
                    ui.Button("Questions", icon: .triangle_alert, role: .plain) {
                        on press: refresh_page(p)
                    }
                    page_menu(b, p, zoom, reading)
                }
                if address.trim() != "" and not looks_like_site(address) {
                    let found = suggestions(b, address)
                    if not found.is_empty() {
                        stack(padding: .related) {
                            ui.Section("From your history") {
                                for v in found {
                                    suggestion_row(p, v)
                                }
                            }
                        }
                    }
                }
                if finding {
                    stack(padding: .related) {
                        ui.TextInput(bind: find_text, placeholder: "Find in page", key: "f") {
                            on submit(typed: Text): find_in_page(p, typed)
                        }
                        if p.find_count > 0 {
                            ui.Text("{p.find_at} of {p.find_count}")
                        }
                        ui.Button("Next", icon: .arrow_down, role: .plain) {
                            on press: find_in_page(p, find_text, next: true)
                        }
                        ui.Button("Close", icon: .x, role: .plain) {
                            on press: stop_finding(p).then(set(finding, false))
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
                    layers(fill) {
                        ui.WebView(title: if p.title == "" { p.url } else { p.title }, uri: p.url) {
                            on navigate(address: Text): went_to(p, address).then(refresh_page(p))
                            on fail(why: Text): set(failed, why)
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
                }
                if let ask = p.dialog {
                    ui.ModalDialog(
                        title: "This page asks",
                        description: .some(ask.origin),
                        open: true,
                        primary: "OK",
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
                        on dismiss: answer_page_dialog(p, ask.request, false).then(refresh_page(p))
                    }
                }
                if let ask = p.sign_in {
                    ui.ModalDialog(
                        title: "Sign in to {ask.site}",
                        description: if ask.retry {
                            .some("The last sign-in was not accepted")
                        } else { .none },
                        open: true,
                        primary: "Sign in",
                        secondary: "Cancel",
                    ) {
                        body: {
                            if ask.realm != "" {
                                ui.Text(ask.realm)
                            }
                            ui.TextInput(
                                bind: sign_user,
                                placeholder: "User name",
                                autofocus: true,
                            ) {
                            }
                            ui.TextInput(bind: sign_password, placeholder: "Password") {
                            }
                        }
                        on act_primary: answer_page_sign_in(
                                p,
                                ask.request,
                                ask.asker,
                                .some(sign_user),
                                .some(sign_password),
                            )
                            .then(refresh_page(p))
                        on dismiss: answer_page_sign_in(p, ask.request, ask.asker, .none, .none)
                            .then(refresh_page(p))
                    }
                }
                if let ask = p.permission {
                    ui.ModalDialog(
                        title: "{ask.origin} asks to use your {ask.permissions}",
                        open: true,
                        primary: "Allow",
                        secondary: "Deny",
                    ) {
                        on act_primary: answer_permission(p, ask.request, true)
                            .then(refresh_page(p))
                        on dismiss: answer_permission(p, ask.request, false).then(refresh_page(p))
                    }
                }
                if not p.held.is_empty() {
                    stack(padding: .group) {
                        ui.Section("Waiting for you") {
                            for ask in p.held {
                                held_row(p, ask)
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

/// One request the page waits on: what it is, and whether it may leave.
view held_row(p: Page, ask: HeldAsk) {
    ui.ListRow(title: "{ask.kind} to {ask.origin}", secondary: ask.method, icon: .clock) {
        on activate: answer_held_request(p, ask.request, true).then(refresh_page(p))
    }
}

/// One page this browser visited that answers what is typed in the address field.
view suggestion_row(p: Page, v: Visit) {
    ui.ListRow(title: v.title, secondary: v.url, icon: .clock) {
        on activate: navigate(p, v.url)
    }
}

/// The pages this browser visited that answer `typed`: its history, never a list of its own.
fn suggestions(b: Browser, typed: Text) -> List<Visit> {
    let q = typed.trim().lower()
    if q == "" {
        return []
    }
    b.visits.filter(v -> v.url.lower().contains(q) or v.title.lower().contains(q)).take(6)
}

/// One step of what was done in a page: what it was, what it acted on and how it ended.
view step_row(s: PageStep) {
    ui.ListRow(
        title: s.operation,
        secondary: step_secondary(s),
        mark: if s.ok { .check } else { .triangle_alert },
    )
}

/// A bookmark's own window: where it is, and the way to take it back.
view bookmark_page(k: Bookmark) {
    std.page(k) {
        content: {
            ui.PropertyList {
                ui.Property("Address", k.url, copyable: true)
                ui.Property("Folder", if k.folder == "" { "Bookmarks" } else { k.folder })
            }
            ui.Button("Remove bookmark", icon: .trash) {
                on press: remove_bookmark(k)
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

/// The library: the pages visited and the pages saved.
view library(b: Browser) {
    state section: Text = "history"
    state query: Text = ""
    let title = if section == "bookmarks" { "Bookmarks" } else { "History" }
    ui.NavShell(content_kind: .list, title: title) {
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

/// The pages saved: each a row that opens where it is.
view bookmarks_list(b: Browser, query: Text) {
    let found = b.bookmarks.filter(
        k -> query == ""
            or k.title.lower().contains(query.lower())
            or k.url.contains(query.lower()),
    )
    if found.is_empty() {
        ui.EmptyState("No bookmarks yet", icon: .star)
    } else {
        std.list(found, in_column: true) { (k, at) ->
            ui.ListItem(item: k, presence: at) {
                bookmark_row(k)
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

/// An address's row in the settings: what the person calls it over the name it fills.
view address_row(a: Address) {
    ui.ListRow(
        title: if a.label == "" { a.full_name } else { a.label },
        secondary: a.full_name,
        mark: .user,
    )
}

/// A remembered form: where it was read, what it asks and how often it was read.
view form_row(f: RememberedForm) {
    ui.DisclosureRow(
        title: if f.label == "" { f.origin } else { f.label },
        secondary: "{f.origin} · read {f.seen} times",
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
