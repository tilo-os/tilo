store-remotes-title = Flatpak repositories
store-remotes-description = Browse software from your configured repositories. Repositories supplied by this device stay available; repositories you add belong to your account.
store-remote-user = This account
store-remote-system = This device
store-remote-enabled = Enabled
store-remote-disabled = Disabled
store-remote-state = { $state }
store-remote-title = { $name } ({ $scope })
store-remote-browse = Browse apps
store-remote-trust-short = Configured repository
store-remote-trust = Configured repository, not reviewed by Tilo
store-remote-trust-explanation = This metadata comes from a configured repository. Tilo has not independently reviewed the app.
store-remote-managed = This repository belongs to the device. Software changes require its administrator.
store-remote-loading = Loading apps from { $name }…
store-remote-results = { $count } apps from { $name }.
store-remote-error = Could not browse repository: { $error }
store-device-version-unavailable = No version from this repository can run on this device and release channel.
store-waiting-for-developer = Waiting for an update from the developer
store-device-provided-version = Version { $version }, provided by this device
store-device-repository-refresh = { $name } needs a repository refresh by this device’s administrator. Other repositories remain available.
store-repository-read-error = Could not read { $name }: { $error }

store-flatpak-unavailable = Flatpak unavailable
store-flatpak-checking = Checking Flatpak availability on this device…
store-remote-enable = Enable source
store-remote-disable = Disable source
store-remote-managed-change = This source is managed by the device. Only sources in your account can be changed here.
store-launch-opened = App opened.

store-technical-details = Technical details
store-show-technical-details = Show technical details
store-hide-technical-details = Hide technical details
store-publisher-key = Publisher key
store-about-app = About this app
store-supported-languages = Supported languages

store-source-timeout = This source took too long to respond. Refresh to try again.
store-refresh-in-progress = Sources are already refreshing. You can keep browsing.
store-catalogue-loading = Loading more apps…
store-catalogue-refreshed = Store refreshed.

# A local archive never authorizes its own publisher.
store-package-size-refused = Choose a Tilo app package smaller than 64 MiB.
store-package-invalid = This file is not a valid signed Tilo package.
store-package-app-only = Choose an app package. System components and runtimes use the system updater.
store-package-offer-required = This package does not match a verified offer from your configured Tilo repositories. Refresh the catalogue or choose a published package.
store-package-unavailable = The package file or app service is unavailable.
store-package-ready = Package verified. Review its details before installing.
store-package-review-required = Review this exact package before installing it.
store-package-installed = Package installed. Open it from its app details.
store-package-choose = Choose a Tilo package
store-package-review = Review package
store-package-install = Install reviewed package
store-package-installing = Installing reviewed package…
store-package-reviewing = Checking package against verified repositories…
store-package-source-required = Choose one package file or handed content identity.
store-package-reviewed-file = Reviewed package file
store-package-reviewed-version = File version
store-package-reviewed-root = Signed package identity
store-package-reviewed-state = Installation
store-package-reinstall = Already installed; the reviewed offer will be verified before installation.
store-package-new-install = Not installed on this device.
store-package-file-type = Tilo package
store-package-review-busy = Too many package reviews are waiting. Finish the current reviews first.

store-package-local-trust = Verified by an existing trusted attestor
store-installed-unanswered = Verified when installed. { $repositories } hasn't answered, so this app's description and updates appear here once it does.
store-installed-unlisted = Verified when installed. No enabled repository lists this app, so it gets no updates here.
store-package-local-description = Signed local application. Installation requires its payload and exact runtime to be available.
store-package-local-source = Local package
store-app-installed = App installed.
store-app-updated = App updated.
store-app-removed = App removed.

store-search-empty = No apps found
store-search-empty-help = Try a different search or category.
store-search-clear = Clear search
store-operation-failed = Could not complete the operation
store-error-refresh = Refresh apps
store-repository-timeout = { $name } did not respond. Other apps remain available. Refresh apps to try again.
store-build-version = Full build version

store-runtime-install = Install runtime
store-runtime-reference = Required runtime
store-runtime-installed = Installed { $name } { $version }. Open the app again to use it.
store-runtime-reviewed = Reviewed { $name }. Installation remains a separate step.
store-runtime-changed = The reviewed runtime package changed; review it again.
store-runtime-service-unavailable = The app service is unavailable. No runtime operation was attempted.
store-runtime-offer-mismatch = The signed runtime offer does not match the requested identity.
store-runtime-offer-ambiguous = More than one repository offers this runtime. Choose one repository before installing.
store-runtime-unavailable = No enabled repository offers the required runtime for this device.

## History: what each change to the reports did

store-draft-report = Started a report about { $app_id }
store-record-moderation-case = The registry opened case { $case_id }: { $status }
store-report-note = Described the concern of a report
store-reports-reports = Edited a report

store-source-partial = Some repositories are unavailable. Other apps remain available.

store-bot-label = BOT
store-bot-kind = Bot
store-bot-what-it-does = What it does
store-bot-appearance = May post under other names and avatars. Always labelled BOT.
store-bot-add-to-room = Add to a room…
store-bot-add-unavailable = Bots run in rooms. Room admins add them from the room's settings.
store-bot-trigger-connector = When its connector reports an event
store-bot-trigger-record-changed = When a record changes
store-bot-trigger-schedule = On a schedule
store-bot-trigger-manual = When someone runs it
store-bot-trigger-incoming-message = When a message arrives
store-runtime-invalid = Invalid runtime repair request.
store-runtime-repair-unavailable = The app service is unavailable. No download was started.
store-runtime-not-installed = The consuming app is not installed with a verifiable package root.
store-runtime-review-required = Review the exact runtime offer before consenting to download.
store-runtime-inspect-route = Use the runtime inspection action through the resource.
store-runtime-busy = A runtime repair is already in progress.
store-runtime-explanation = Add the component this app needs. No component is downloaded until you agree.
store-runtime-consumer = App
store-runtime-title = Component repair
store-runtime-show = Component repair
store-runtime-repair-reference = Required component
store-runtime-source = Signed repository
store-runtime-package = Package
store-runtime-version = Package version
store-runtime-size = Signed download size
store-runtime-publisher = Publisher identity
store-runtime-root = Package root
store-runtime-hash = Archive hash
store-runtime-consent = Only this component will be downloaded. Its signature and contents are checked before use.
store-runtime-download = Download and add
store-runtime-inspecting = Checking the component…
store-runtime-repair-reviewed = Component review ready.
store-runtime-acquiring = Downloading and verifying the component…
store-runtime-background = Back leaves this work running in the background. It does not cancel or undo installation.
store-runtime-installing = Download complete. Verifying and installing…
store-runtime-downloaded = Downloaded { $received } of { $total } bytes.
store-runtime-downloaded-unknown = Downloaded { $received } bytes.
store-runtime-settled = Component repair finished.
store-runtime-complete = Component verified on this device.
store-runtime-reopen = Close the app and open it again to use the component.
store-runtime-failed = Component repair failed
store-runtime-retry = Review again

## The catalogue

store-view-home = Discover
store-view-updates = Updates
store-view-updates-count = Updates ({ $count })
store-view-installed = On this device
store-view-bots = Bots
store-views-label = Store views
store-search-placeholder = Search apps and bots
store-search-title = Search
store-search-apps = Apps
store-search-more-flathub = More on Flathub
store-show-more = Show more
store-featured-title = Featured
store-recent-title = Recently updated
store-categories-title = Categories
store-category-empty = No apps in this category
store-updates-title = Updates
store-see-all-updates = See all
store-update-all = Update all
store-update-to = Version { $version }
store-updates-none = Every app is up to date
store-installed-none = No apps on this device yet
store-bots-none = Your repositories list no bots
store-loading = Loading apps
store-empty-title = No apps available
store-empty-help = Add or enable a repository you trust.
store-offline-title = Repositories offline
store-working-title = Working
store-working = Working in the background…
store-trust-title = Trust and moderation
store-source-tilo = Tilo
store-source-flathub = Flathub

store-category-accessibility = Accessibility
store-category-audiovideo = Audio and video
store-category-multimedia = Audio and video
store-category-development = Developer tools
store-category-education = Education
store-category-game = Games
store-category-games = Games
store-category-graphics = Graphics and design
store-category-network = Internet
store-category-office = Office
store-category-productivity = Productivity
store-category-science = Science
store-category-system = System
store-category-utility = Utilities
store-category-utilities = Utilities
store-category-bots = Bots

## An app's one action and its progress

store-action-open = Open
store-action-install = Install
store-action-update = Update
store-action-remove = Remove
store-action-keep = Keep on this device
store-action-release = Allow cleanup
store-action-flatpak-setup = Install Flatpak first
store-action-view = View
store-action-tooltip = { $action } { $name }
store-kept = { $name } is kept on this device.
store-released = { $name } may be cleaned up when it goes unused.
store-progress-opening = Opening { $name }…
store-progress-installing = Installing { $name }…
store-progress-updating = Updating { $name }…
store-progress-removing = Removing { $name }…
store-progress-keeping = Keeping { $name } on this device…
store-progress-releasing = Allowing cleanup of { $name }…
store-progress-preparing = Preparing Flatpak…
store-progress-adding = Adding { $name }…
store-update-all-starting = { $count ->
    [one] Updating 1 app…
   *[other] Updating { $count } apps…
}
store-update-all-progress = Updating { $index } of { $total }: { $name }…
store-update-all-done = { $total ->
    [one] Updated 1 app.
   *[other] Updated { $total } apps.
}
store-update-all-failed = Updated { $updated } of { $total } { $total ->
    [one] app
   *[other] apps
}; { $first }
store-update-all-failed-more = Updated { $updated } of { $total } { $total ->
    [one] app
   *[other] apps
}; { $first } (and { $more } more failed)

## An app's page

store-screenshots-title = Screenshots
store-screenshot-label = { $name } screenshot { $number }
store-whats-new = What's new
store-release-version = Version { $version }
store-facts-title = Facts
store-fact-download = Download size
store-fact-installed-size = Installed size
store-fact-source = Source
store-fact-source-tilo = Tilo, from { $registry }
store-fact-license = Licence
store-fact-developer = Developer
store-fact-publisher = Publisher
store-fact-version = Version
store-fact-updated = Updated
store-fact-categories = Categories
store-fact-homepage = Website
store-fact-source-code = Source code
store-fact-on-device = On this device
store-open-homepage = Open website
store-open-source-code = Open source code
store-app-id = App ID
store-package-digest = Package digest
store-trust-section = Trust
store-trust-publisher-signed = Publisher identity and package integrity are verified. This is not an independent quality review.
store-trust-flathub = Tilo has not independently reviewed this app.

store-device-on-demand = Downloads when you open it
store-device-cached = Downloaded, not kept
store-device-kept = Kept on this device
store-device-system = Part of the system
store-device-cached-version = Version { $version }, downloaded, not kept
store-device-kept-version = Version { $version }, kept on this device
store-device-system-version = Version { $version }, part of the system
store-device-not-installed = Not installed
store-device-installed-version = Version { $version } installed

store-permissions-title = What it can do
store-permissions-none = Asks for nothing beyond its own window.
store-permissions-tilo-sandbox = Runs in Tilo's sandbox and asks before it uses your files, camera or microphone.
store-permissions-unknown = Flathub has not said what this app can do.
store-permission-leaves-sandbox = Can run programs outside its sandbox
store-permission-all-files = Reads and changes all your files
store-permission-home = Reads and changes your home folder
store-permission-folder = Reads and changes your { $folder } folder
store-permission-chosen-files = Opens and saves files you choose
store-permission-internet = Uses the internet
store-permission-camera = Uses the camera
store-permission-microphone = Uses the microphone
store-permission-sound = Plays and records sound
store-permission-screen-capture = Captures your screen
store-permission-location = Uses your location
store-permission-all-devices = Uses all devices, such as cameras and game controllers
store-permission-bluetooth = Uses Bluetooth
store-permission-legacy-display = Uses the older X11 display, which other apps can watch
store-permission-system-services = Talks to system services
store-permission-secrets = Keeps secrets in your keyring
store-permission-ssh-keys = Uses your SSH or GPG keys
store-permission-printing = Prints
store-permission-clipboard = Reads and changes the clipboard
store-permission-notifications = Shows notifications
store-permission-runs-programs = Runs programs
store-permission-opens-links = Opens links in other apps
store-folder-downloads = Downloads
store-folder-documents = Documents
store-folder-music = Music
store-folder-pictures = Pictures
store-folder-videos = Videos
store-folder-desktop = Desktop
store-folder-public = Public
store-folder-templates = Templates

## Blocking

store-block-title = Block
store-block-button = Block app
store-block-tooltip = Block { $name }
store-block-explanation = A blocked app appears nowhere in the Store or in search, and cannot be opened or installed from them. Only you can unblock it.
store-blocked-title = Blocked
store-blocked-by-you = You blocked this app.
store-blocked-by-admin = { $name } blocked this app on this device.
store-blocked-by-policy = This device's administrator blocked this app.
store-blocked-explanation = It appears nowhere in the Store or in search.
store-blocked-refused = This app is blocked.
store-unblock = Unblock
store-blocked-by-workspace = The { $workspace } workspace blocked this app.
store-block-unavailable = Your block list could not be saved. Try again.
store-unblock-not-yours = Only whoever blocked this app can unblock it.
store-unblock-not-blocked = This app is not blocked.

## Bots

store-bot-access-title = What it can reach
store-bot-access-none = Nothing beyond the room it runs in.
store-bot-uses-app = Uses its actions and watches its changes
store-bot-needs-connector = Connector, version { $version } or newer

## Reports

store-report-title = Report a problem
store-report-explanation = Report a concern together with your workspace, or read how decisions and appeals work.
store-report-create = Create report draft
store-report-create-tooltip = Start a report draft your workspace shares
store-report-how = How reports work
store-report-export = Export evidence
store-report-export-tooltip = Save the report's evidence as a JSON file
store-report-refresh = Refresh status
store-report-appeal = Appeal decision
store-report-submit = Submit report
store-report-submit-tooltip = Sign the report and send it to the app's repository
store-report-case = Moderation case
store-report-draft = Report draft
store-report-note-label = What happened?
store-report-note-help = Describe what you saw, with evidence another reviewer can check.
store-report-note-placeholder = Describe what happened
store-reports-none = No report drafts
store-reports-none-help = Open an app and choose Create report draft.
store-trust-reports-title = How reports work
store-trust-reports = Reports are signed with your workspace identity and stay drafts your workspace shares until you submit them. A new status shows only once it is verified, so an offline or refused request stays a draft you can send again.
store-trust-signature-title = What a signature means
store-trust-signature = A verified package comes from its publisher and has not been changed. That does not make an app useful, private or honest, and the Store never invents a review badge.
store-trust-guidelines-title = Publishing guidelines
store-trust-guidelines = Listings must describe what the app does, name any paid or online services it depends on, ask only for the permissions it needs, keep people's data across updates, work with assistive technology, and never impersonate others or track people without saying so.

## Flatpak repositories

store-remote-add-title = Add a repository
store-remote-add = Add repository
store-remote-refresh = Refresh repositories
store-remote-name = Name
store-remote-name-placeholder = Repository name
store-remote-url = Repository URL
store-remote-url-help = Repositories must use HTTPS.
store-remote-remove = Remove repository
store-remotes-configured = Configured repositories
