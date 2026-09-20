# Guided setup semantic resource and action labels.
imager-setup-action-choose-experience = Choose setup experience
imager-setup-placeholder-experience = desktop
imager-setup-param-experience = Available setup experience
imager-setup-action-set-common-mode = Set common folder mode
imager-setup-action-set-folder-mode = Set folder mode
imager-setup-placeholder-folder-mode = copy
imager-setup-param-folder-mode = Folder behavior
imager-setup-param-common-folder-mode = Folder behavior applied to folders without an override
imager-setup-action-use-common-mode = Use the common folder mode
imager-setup-action-remove-folder = Remove folder from setup
imager-setup-action-select-suggestion = Select suggested folder
imager-setup-action-choose-folder = Choose another folder
imager-setup-action-toggle-help = Show or hide folder help
imager-setup-action-next = Continue setup
imager-setup-action-back = Go back in setup
imager-setup-action-open-imports = Open folder imports
imager-setup-action-install-desktop = Install Tilo Desktop
imager-setup-action-cancel-desktop-download = Cancel Desktop download
imager-setup-action-retry-desktop = Retry Desktop host check
imager-setup-action-start-download = Download Tilo
imager-setup-action-pause-download = Pause download
imager-setup-action-resume-download = Resume download
imager-setup-action-cancel-download = Cancel download
imager-setup-action-discard-download = Discard unfinished download data
imager-setup-action-release-download = Keep download for later

imager-setup-error-no-experience = No setup experience is available on this computer
imager-setup-error-unknown-experience = Unknown setup experience: { $value }
imager-setup-error-unknown-mode = Unknown folder mode: { $value }
imager-setup-error-invalid-operation = Invalid setup operation: { $error }
imager-setup-error-folder-choices-closed = Folder choices are not open on the current setup page
imager-setup-error-folder-selection-stale = The folder selection is no longer available
imager-setup-error-connected-unavailable = Folder syncing is not available in this build yet
imager-setup-error-wait-for-scans = Wait for the selected folders to finish scanning
imager-setup-error-suggestion-unavailable = That suggested folder is unavailable on this computer
imager-setup-error-picker-unavailable = The folder picker is already open or folder choices are not available
imager-setup-error-picker-open = The folder picker is already open
imager-setup-error-help-not-on-page = Folder help is available on the folder page
imager-setup-error-cannot-continue = Complete the current setup choice before continuing
imager-setup-error-at-start = Setup is already on its first page
imager-setup-error-desktop-install-unavailable = Desktop installation is not available in the current setup state
imager-setup-error-desktop-download-not-active = The Desktop package download is not active
imager-setup-error-desktop-retry-unavailable = The Desktop host check cannot be retried in the current setup state
imager-setup-error-download-start-unavailable = A Full Tilo download cannot start in its current state.
imager-setup-error-download-review-only = Full Tilo downloads can start only from the Full Tilo review.
imager-setup-error-download-pause-unavailable = The download is not currently active
imager-setup-error-download-resume-unavailable = This saved download cannot be resumed yet
imager-setup-error-download-cancel-unavailable = The download is not currently active
imager-setup-error-download-discard-unavailable = Unfinished download data is not available to discard
imager-setup-error-download-release-unavailable = The verified download is not available to keep for later

imager-setup-resource-root = Guided Tilo setup
imager-setup-resource-selection = Current setup selection
imager-setup-resource-review = Setup review state
imager-setup-resource-availability = Current setup availability
imager-setup-resource-selected-folder = Selected folder { $number }
imager-setup-resource-suggested-folder = { $folder } folder
imager-setup-resource-desktop = Tilo Desktop installation
imager-setup-resource-internal-download = Full Tilo download
imager-setup-badge-selection = Selection
imager-setup-badge-review = Review
imager-setup-badge-availability = Availability
imager-setup-badge-folder = Folder { $number }
imager-setup-badge-suggestion = Suggested folder
imager-setup-badge-desktop = Desktop
imager-setup-badge-download = Download
imager-setup-badge-page-experience = Experience
imager-setup-badge-page-folders = Folders
imager-setup-badge-page-review = Review
imager-setup-badge-page-setup = Setup
imager-setup-suggestion-available = Available to select
imager-setup-suggestion-unavailable = Unavailable on this computer

imager-setup-detail-folder-common = { $mode } · common
imager-setup-detail-folder-override = { $mode } · override
imager-setup-detail-desktop = { $phase } · { $status }
imager-setup-detail-internal-download = { $phase } · { $status }
imager-setup-detail-selection =
    experience { $experience ->
        [none] no experience selected
       *[other] { $experience }
    } · common { $common_mode } · { $folder_count } folders · { $active_folder_count } active · help { $help ->
        [true] open
       *[false] closed
    } · imports { $imports ->
        [true] open
       *[false] closed
    }
imager-setup-detail-review =
    ready { $ready ->
        [true] yes
       *[false] no
    } · current page { $page }
imager-setup-detail-availability =
    Desktop { $desktop ->
        [true] yes
       *[false] no
    } · Full OS { $full_os ->
        [true] yes
       *[false] no
    } · Connected { $connected ->
        [true] yes
       *[false] no
    } · Next { $next ->
        [true] yes
       *[false] no
    } · Back { $back ->
        [true] yes
       *[false] no
    } · Folder policies { $folder_policies ->
        [true] yes
       *[false] no
    } · Folder edits { $folder_edits ->
        [true] yes
       *[false] no
    } · Help { $help ->
        [true] yes
       *[false] no
    } · Imports { $open_imports ->
        [true] yes
       *[false] no
    }
imager-setup-detail-root =
    { $page } · { $experience ->
        [none] no experience selected
       *[other] { $experience }
    } · { $common_mode } · { $folder_count } folder(s) · review { $review ->
        [true] yes
       *[false] no
    } · Desktop { $desktop ->
        [true] yes
       *[false] no
    } · Full OS { $full_os ->
        [true] yes
       *[false] no
    } · Connected { $connected ->
        [true] yes
       *[false] no
    } · Next { $next ->
        [true] yes
       *[false] no
    } · Back { $back ->
        [true] yes
       *[false] no
    }

# Folder import semantic resource and action labels.
imager-import-resource-root = Folder imports
imager-import-resource-operation = Folder import
imager-import-resource-folder = Saved folder { $number }
imager-import-badge-root = Imports
imager-import-badge-operation = Copy
imager-import-badge-folder = Saved folder
imager-import-action-choose-source = Choose source folder
imager-import-action-read-status = Read current import status
imager-import-action-choose-destination = Choose destination folder
imager-import-action-clear-selection = Clear folder selection
imager-import-action-prepare-review = Prepare copy review
imager-import-action-check-identity = Check identity setup
imager-import-action-start-reviewed-folders = Copy reviewed folders
imager-import-action-later-all = Decide later for all folders
imager-import-action-start = Start copy
imager-import-action-retry = Continue copy
imager-import-action-cancel = Stop copy
imager-import-action-toggle-details = Show or hide copy details
imager-import-action-next-issues = Load next issue page
imager-import-action-first-issues = Return to first issue page
imager-import-action-review-destination = Review destination
imager-import-action-choose-folder-destination = Choose another destination
imager-import-action-folder-copy = Use one-time Copy
imager-import-action-folder-later = Decide later
imager-import-action-folder-connected = Use Connected folders
imager-import-error-invalid-operation = Invalid folder import operation: { $error }
imager-import-error-operation-kind = Folder import operations are local view operations
imager-import-error-picker-unavailable = A folder chooser is already open or unavailable
imager-import-error-selection-empty = No folder selection is available to clear
imager-import-error-review-unavailable = Choose a source and destination before preparing a review
imager-import-error-identity-check-unavailable = Identity setup is already ready or being checked
imager-import-error-batch-unavailable = No reviewed Copy folder is ready to start
imager-import-error-later-all-unavailable = Saved folder choices cannot change while a copy is running
imager-import-error-start-unavailable = This copy is not ready to start
imager-import-error-retry-unavailable = This copy cannot be continued in its current state
imager-import-error-cancel-unavailable = This copy is not currently running
imager-import-error-details-unavailable = This copy has no details to show
imager-import-error-next-issues-unavailable = Another issue page is not available
imager-import-error-first-issues-unavailable = The first issue page is not available
imager-import-error-folder-review-unavailable = This saved folder cannot be reviewed in its current state
imager-import-error-folder-mode-unavailable = Saved folder choices cannot change while a copy is running
imager-import-error-connected-unavailable = Connected folders are not available in this build yet
imager-import-error-stale-operation = That folder import is no longer available
imager-import-error-stale-folder = That saved folder choice is no longer available
imager-import-error-command-refused = The folder import action could not be completed because its current state changed
imager-import-detail-root = { $phase } · { $operations } import(s) · error { $error }
imager-import-detail-operation = { $state } · { $files } verified file(s) · { $bytes } verified bytes · { $issues } issue(s)
imager-import-detail-folder = { $mode } · destination reviewed { $reviewed }

# Connected folders management. Native changes remain unavailable until the
# device-local qualification and consent gates are complete.
imager-connected-resource-entry = Connected folders
imager-connected-resource-root = Connected folders
imager-connected-resource-row = Saved connection { $number }
imager-connected-badge-entry = Connections
imager-connected-badge-root = Connections
imager-connected-badge-row = Connection
imager-connected-badge-selected-row = Selected connection
imager-connected-detail-entry = View saved connections and their current device-local status
imager-connected-import-title = Connected folders
imager-connected-import-detail = View saved connections and current status. Opening management does not start folder changes.
imager-connected-detail-root = { $phase } { $connections } saved connection(s).
imager-connected-detail-phase-loading = Loading saved connections.
imager-connected-detail-phase-unavailable = Saved connections are unavailable.
imager-connected-detail-phase-empty = No saved connections are available.
imager-connected-detail-phase-catalog = Viewing saved connections.
imager-connected-detail-phase-settings = Viewing one saved connection.

imager-connected-action-open-management = Open Connected folders
imager-connected-action-back = Back to folder imports
imager-connected-action-observe = Read current connection status
imager-connected-action-reload = Reload saved connections
imager-connected-action-first = Return to first connection page
imager-connected-action-next = Load next connection page
imager-connected-action-open-settings = Open connection settings
imager-connected-action-sync-now = Sync now
imager-connected-action-pause = Pause for this session
imager-connected-action-disconnect = Disconnect folders
imager-connected-action-accept-initial = Approve initial connection
imager-connected-action-restore = Restore selected version
imager-connected-action-replace = Replace displayed target

imager-connected-error-invalid-operation = Invalid Connected folders operation: { $error }
imager-connected-error-operation-kind = The Connected folders action used the wrong operation kind
imager-connected-error-stale-route = That Connected folders view is no longer current
imager-connected-error-stale-page = That saved connection page is no longer current
imager-connected-error-stale-connection = That saved connection is no longer available
imager-connected-error-request-pending = Wait for the current Connected folders request to finish
imager-connected-error-first-unavailable = The first connection page cannot be loaded now
imager-connected-error-next-unavailable = Another saved connection page is not available
imager-connected-error-open-pending = Connected folders are still opening
imager-connected-error-open-worker = Connected folders could not start opening: { $error }
imager-connected-error-switch-busy = Finish the current connection observation or review before opening another connection
imager-connected-error-service-unavailable = Connected folders are not open on this computer
imager-connected-error-service = Connected folders could not continue: { $error }
imager-connected-error-native-capability-closed = Changes to Connected folders are unavailable in this version

imager-connected-title = Connected folders
imager-connected-intro = View saved connections and their current device-local status. Connected keeps a local copy that uses local storage.
imager-connected-loading-title = Opening Connected folders
imager-connected-loading-body = Reading saved connection metadata. No folder change starts while this page opens.
imager-connected-unavailable-title = Connected folders unavailable
imager-connected-unavailable-body = Saved connection metadata cannot be opened here. No folder change has started.
imager-connected-empty-title = No saved connections
imager-connected-empty-body = No saved connection records were found. Creating a connection is not available in this version.
imager-connected-catalog-title = Saved connections
imager-connected-row-title = Connection { $number }
imager-connected-row-active-initial = Saved connection · initial comparison needed
imager-connected-row-active-compared = Saved connection · prior comparison recorded
imager-connected-row-disconnecting = Saved connection · disconnect is still finishing
imager-connected-row-disconnected = Saved connection · disconnected
imager-connected-row-saved = Saved connection
imager-connected-settings-title = Connection settings
imager-connected-source = Source: { $path }
imager-connected-destination = Destination: { $path }
imager-connected-steps = Completed verified comparison steps: { $steps }
imager-connected-pending-operation = A folder change is still being checked.
imager-connected-endpoint-issue = { $side }: { $error }
imager-connected-awaiting-review = Initial comparison advertises { $entries } entries. Conflict and support classification is unavailable; approval remains closed.
imager-connected-summary-unavailable = A complete conflict and unsupported-item summary is unavailable. Initial approval remains unavailable.
imager-connected-retention-policy = Deleted or replaced versions remain recoverable for 30 days; items that still require preservation may remain longer.
imager-connected-native-closed = Sync, pause, disconnect, approval, restore and replace are unavailable in this version. Viewing status does not start or approve changes.

imager-connected-status-not-open = Connected folders have not been opened yet.
imager-connected-status-loading = Opening the saved connection store…
imager-connected-status-loading-catalog = Reading saved connections…
imager-connected-status-opening-settings = Opening current settings without starting synchronization…
imager-connected-status-empty = No saved connection records were found.
imager-connected-status-catalog = Saved connection metadata is current.
imager-connected-status-unsupported-host = Connected folders are unavailable on this operating system.
imager-connected-status-busy = Connected folders are busy. Current information remains visible; reload explicitly later.
imager-connected-status-closed = The Connected folders service closed. Reopen explicitly to observe it again.
imager-connected-status-pending-not-compared = This connection has not been compared yet.
imager-connected-status-pending-incomplete-observation = The last comparison observation was incomplete.
imager-connected-status-pending-native-refused = This computer refused the connection during its safety checks.
imager-connected-status-pending-structural = A folder structure operation is not supported yet.
imager-connected-status-pending-run-limit = The comparison stopped at its current safety limit.
imager-connected-status-scanning = Comparing the two folders.
imager-connected-status-syncing = Applying verified folder changes.
imager-connected-status-awaiting-review = The complete initial comparison still needs authoritative review.
imager-connected-status-up-to-date = The latest completed observation is up to date.
imager-connected-status-conflict = A conflict needs review.
imager-connected-status-paused = Paused for this session only.
imager-connected-status-disconnecting = Disconnect is still reaching a durable boundary.
imager-connected-status-disconnected = This saved connection is disconnected.
imager-connected-status-detached = This observation is detached. Reopen it explicitly.
imager-connected-status-unavailable = { $side }: { $error }
imager-connected-status-recovery-required = Recovery needs attention: { $error }
imager-connected-status-other-error = Connected folders could not continue: { $detail }

imager-connected-side-source = Source folder
imager-connected-side-destination = Destination folder
imager-connected-reason-unsupported-host = This operating system does not support Connected folders.
imager-connected-reason-busy = Another Connected folders request is still running.
imager-connected-reason-closed = The Connected folders service closed.
imager-connected-reason-limit-exceeded = The safe processing limit was reached.
imager-connected-reason-stale-review = The earlier review is no longer current.
imager-connected-reason-disconnected = This saved connection is disconnected.
imager-connected-reason-identity-changed = A folder identity changed since the last observation.
imager-connected-reason-unavailable = The folder is temporarily unavailable.
imager-connected-reason-permission-denied = The folder cannot be accessed with current permissions.
imager-connected-reason-unsafe-volume = The storage volume does not meet the required safety checks.
imager-connected-reason-read-only = The folder is on read-only storage.
imager-connected-reason-unverified = The latest folder observation could not be verified.
imager-connected-reason-unstable = The folder changed while it was being observed.
imager-connected-reason-recovery-required = Saved recovery information must be reviewed.
imager-connected-reason-recovery-space = Recovery storage needs attention before this connection can continue.
imager-connected-reason-corrupt-store = Saved connection metadata is damaged.
imager-connected-reason-persistence = Saved connection metadata could not be updated.
imager-connected-reason-unknown = An unexpected Connected folders error occurred.
