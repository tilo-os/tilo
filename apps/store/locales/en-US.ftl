store-remotes-title = Flatpak repositories
store-remotes-description = Browse software from your configured repositories. Repositories supplied by this device stay available; repositories you add belong to your account.
store-remote-user = This account
store-remote-system = This device
store-remote-enabled = Enabled
store-remote-disabled = Disabled
store-remote-state = { $scope } · { $state }
store-remote-title = { $name } ({ $scope })
store-remote-browse = Browse apps
store-remote-trust-short = Configured repository
store-remote-trust = Configured repository · no Tilo review
store-remote-trust-explanation = This metadata comes from a configured repository. Tilo has not independently reviewed the app.
store-remote-managed = This repository belongs to the device. Software changes require its administrator.
store-remote-loading = Loading apps from { $name }…
store-remote-results = { $count } apps from { $name }.
store-remote-error = Could not browse repository: { $error }
store-device-version-unavailable = No version from this repository can run on this device and release channel.
store-device-provided-version = Provided by this device · version { $version }
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
store-package-local-description = Signed local application. Installation requires its payload and exact runtime to be available.
store-package-local-source = Local package
store-app-installed = App installed.
store-app-updated = App updated.
store-app-removed = App removed.

store-search-empty = No apps found
store-search-empty-help = Try a different search or category.
store-search-clear = Clear filters
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
