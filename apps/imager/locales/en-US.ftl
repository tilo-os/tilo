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
imager-import-error-command-refused = Folder import could not continue. Open import status (read-status) on this computer for details
imager-import-detail-root = { $phase } · { $operations } import(s) · error { $error }
imager-import-detail-operation = { $state } · { $files } verified file(s) · { $bytes } verified bytes · { $issues } issue(s)
imager-import-detail-folder = { $mode } · destination reviewed { $reviewed }
