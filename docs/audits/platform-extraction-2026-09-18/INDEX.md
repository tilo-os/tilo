# Coverage and finding register

See [README.md](README.md) for scope, destination architecture, implementation work packages, migration order and verification requirements.

## Coverage

| Domain report | Finding groups |
| --- | ---: |
| [01-apps-productivity ](01-apps-productivity.md) | 13 |
| [02-apps-media ](02-apps-media.md) | 14 |
| [03-apps-communication ](03-apps-communication.md) | 13 |
| [04-apps-system ](04-apps-system.md) | 22 |
| [05-imager ](05-imager.md) | 12 |
| [06-shell-state ](06-shell-state.md) | 18 |
| [07-shell-backends ](07-shell-backends.md) | 8 |
| [08-runtime-packaging ](08-runtime-packaging.md) | 24 |
| [09-system ](09-system.md) | 16 |
| [10-collab-identity ](10-collab-identity.md) | 11 |
| [11-media-browser-text ](11-media-browser-text.md) | 11 |
| [12-other-crates ](12-other-crates.md) | 10 |
| [13-filesystem ](13-filesystem.md) | 8 |
| [14-os-tooling ](14-os-tooling.md) | 7 |
| [15-vendor-site ](15-vendor-site.md) | 7 |

Total: **15 reports, 194 reviewed finding groups**. Groups include extraction work, intentional platform backends, tooling/test boundaries and false positives; this is not a count of bugs.

## Finding register

### [01-apps-productivity](01-apps-productivity.md)

- **A01-01** — Per-app host directory discovery (extract; medium)
- **A01-02** — Calendar hardcodes the Downloads folder (extract; medium)
- **A01-03** — Clocks implements authenticated-session transport and private writes (extract backend; high)
- **A01-04** — Contacts directly speaks a custom notification D-Bus extension (extract backend; high)
- **A01-05** — TOTP chooses key custody using secret-tool availability (extract backend; high)
- **A01-06** — Code editor owns Unix subprocess-group lifecycle (extract backend; high)
- **A01-07** — Ebook equates sharing with a Linux mount path and POSIX path-list syntax (extract; high)
- **A01-08** — PDFium dynamic library discovery is Linux-centric (extract loader backend; medium)
- **A01-09** — PDF save requires directory fsync with ordinary File::open (extract durable-file operation; high)
- **A01-10** — Word processor guesses toolkit drag payload ownership (extract ownership capability; high)
- **A01-11** — Word processor discovers locale from POSIX environment itself (extract discovery; medium)
- **A01-12** — Platform-dependent test fixtures need capability-specific verification (test-only; medium)
- **A01-13** — Linux packaging and macOS bundle tooling are intentional target backends (retain target-specific)

### [02-apps-media](02-apps-media.md)

- **A02-01** — Camera owns Linux V4L2 enumeration and capture (high)
- **A02-02** — Camera launches xdg-open directly (high)
- **A02-03** — Maps embeds the XDG location implementation (high)
- **A02-04** — Music network policy probes NetworkManager itself (medium)
- **A02-05** — Music duplicates native playback and media-session backends (high)
- **A02-06** — Editor owns native decode, PCM output and export plumbing (high)
- **A02-07** — Tube private persistence contains Unix security primitives (high)
- **A02-08** — Tube explicitly binds account/device secret acquisition to XDG (high)
- **A02-09** — Multiple apps duplicate platform directory selection (medium)
- **A02-10** — Editor resolver hardcodes FUSE mount and equates stat with hydration (high)
- **A02-11** — Music hand-builds file URIs (medium)
- **A02-12** — yt-dlp availability/execution is a host integration requirement (medium)
- **A02-13** — Native-media test environment assumptions must remain explicit
- **A02-14** — App manifests intentionally describe Linux deployment

### [03-apps-communication](03-apps-communication.md)

- **A03-01** — External URI/file launching belongs to a host capability (high)
- **A03-02** — Terminal owns the Unix service discovery and descriptor transport (high)
- **A03-03** — Terminal shell/PTY/confinement probing is a platform backend (high)
- **A03-04** — Agent diagnostic endpoints expose Unix transport (medium)
- **A03-05** — Chat capture selection and media availability are Linux-shaped (high)
- **A03-06** — Chat host identity and workspace mount discovery (medium)
- **A03-07** — Browser private storage safety relies on Unix permissions, flock and PID liveness (high)
- **A03-08** — Repository-owned Chromium native host is already a backend, located under an app (high)
- **A03-09** — macOS Keychain implementation belongs behind credential-store capability (high)
- **A03-10** — Remote SSH trust-store and default identity paths assume Unix layout (high)
- **A03-11** — App shortcut OS choice and Git credential bridge (medium)
- **A03-13** — Browser private process launch and WPE composition remain app-owned (medium)
- **A03-12** — Keep native deployment/test boundaries explicit (medium)

### [04-apps-system](04-apps-system.md)

- **A04-01** — Files owns the collaborative filesystem's xattr transport and errno ABI (high)
- **A04-02** — Files Unix service channels and path-byte encoding live in app code (high)
- **A04-03** — Files trash, known folders, and symlinks contain target policy (high)
- **A04-04** — Files launch integration and drag handoff assume the Tilo host layout (medium)
- **A04-05** — Workspace and user-directory defaults are duplicated in apps (medium)
- **A04-06** — OSK has an unguarded Wayland/Linux injection backend (high)
- **A04-07** — OSK native swipe model asset discovery is host-specific (low)
- **A04-08** — Disks contains Linux block-device discovery and privileged operations (high)
- **A04-09** — Firewall's firewalld backend is app-local (medium)
- **A04-10** — Logs owns journalctl execution/parsing (medium)
- **A04-11** — Monitor mixes portable sampling with Linux enrichment and POSIX termination (medium)
- **A04-12** — Containers owns Podman/Distrobox backend and shell launcher scripts (high)
- **A04-13** — VPN telemetry and local profile persistence retain Linux assumptions (medium)
- **A04-14** — Settings embeds host identity, account, security and hardware discovery (high)
- **A04-15** — Settings audio extends os-actions with direct wpctl logic (medium)
- **A04-16** — Settings input-source/timezone catalogs and fallback paths are native (medium)
- **A04-17** — Settings embeds Bootc/systemd deployment management (high)
- **A04-18** — Settings device mesh and local model admin owns Linux helper locations/processes (high)
- **A04-19** — Store has host Flatpak probes outside its existing app-host broker (medium)
- **A04-20** — Windows app owns Linux appliance helpers and Unix service handle adoption (high)
- **A04-21** — Backup metadata and private temporary output use Unix-specific primitives (medium)
- **A04-22** — Preserve existing shared boundaries; target-scope deployment and native certification (medium migration coordination)

### [05-imager](05-imager.md)

- **A05-01** — Device inventory, identity and destructive media authorization (high; backend + extract)
- **A05-02** — Host architecture and build target are conflated outside Windows (medium; extract)
- **A05-03** — Native pickers and known folders live inside the app (medium; backend)
- **A05-04** — Local storage roots and protected directory policy leak into view/flow (medium; extract)
- **A05-05** — Desktop installation is a deliberate Linux service backend (high; backend)
- **A05-06** — Private filesystem ownership, durable receipts and native names (high; backend + mixed seams)
- **A05-07** — Verified persistent download cache and bundle handle operations (high; backend)
- **A05-08** — Connected-folder endpoint evidence and NTFS source bridge (high; backend)
- **A05-09** — Transaction witnesses, leases and durable connection storage (high; backend)
- **A05-10** — Build graph and standalone profile tool need a narrow shared codec seam (medium; extract)
- **A05-11** — Packaging and credentials are target deployment tooling (retain; deployment)
- **A05-12** — Native tests must move with backend ownership, not disappear (high; test)

### [06-shell-state](06-shell-state.md)

- **A06-01** — File-search fallback directly invokes Linux desktop opener
- **A06-02** — Host-app catalogue and launch model expose desktop-entry backend
- **A06-03** — Process launch, service descriptors and runtime directories mixed with shell policy
- **A06-04** — General brokers bind Unix transport and peer identity directly
- **A06-05** — Native runtime host already has a Unix seam, but daemon and MCP bypass it
- **A06-06** — Secure persistence primitives duplicated across domain modules
- **A06-07** — Onboarding UI owns privileged enrollment processes and mount discovery
- **A06-08** — First boot, config and machine identity encode Linux deployment locations
- **A06-09** — Filesystem publication helpers hard-coded into state and network actor
- **A06-10** — Nearby transfer and cache space use OS filesystem implementation
- **A06-11** — Topbar performs Linux process liveness and private file IPC during draw
- **A06-12** — Hardware capabilities and device class selected in shell
- **A06-13** — Remote audio and capture should reuse existing target adapter seams
- **A06-14** — Desktop service adapters live with notification/media/Bluetooth models
- **A06-15** — Projection, catalogue and Windows compatibility select packaged helpers
- **A06-16** — Compositor/network graphics are deliberate platform backend
- **A06-17** — Entry points are already appropriate platform composition roots
- **A06-18** — Test/platform suite separation and pre-existing hardware skip constraints

### [07-shell-backends](07-shell-backends.md)

- **A07-01** — Extract Unix local transport from portable control/capture contracts
- **A07-02** — Host paths and private persistence are embedded in shared portal services
- **A07-03** — Retain Wayland/DRM compositor as explicit Linux platform backend
- **A07-04** — Keep sandbox enforcement Linux-specific; eliminate misleading duplicate construction APIs
- **A07-05** — Host service implementations should plug into existing semantic traits
- **A07-06** — Portal protocol is Linux; capture/control authority is not
- **A07-07** — Cargo and native bundle boundaries
- **A07-08** — Test tools/examples carry intentional native prerequisites

### [08-runtime-packaging](08-runtime-packaging.md)

- **A08-01** — Service discovery and connected-handle transport
- **A08-02** — Automatic native attachment mixes bootstrap policy with Unix exec/build machinery
- **A08-03** — Granted filesystem implementation is a valuable existing security seam
- **A08-04** — Storage service escapes the guarded native filesystem boundary
- **A08-05** — XDG portals are a deliberate backend, not the portable capability API
- **A08-06** — Identity private storage has an existing provider contract to reuse
- **A08-07** — ush is an entire intentional Linux runtime backend
- **A08-08** — Launcher security machinery must move as a coherent Linux backend
- **A08-09** — Launcher shared modules still expose Unix IPC and Linux environment plans
- **A08-10** — GPU selection, runtime hardening and performance are host adapters
- **A08-11** — App-host daemon mixes package operations, Linux launchers and service-manager plumbing
- **A08-12** — app-fs is an intentional FUSE adapter over reusable verified content storage
- **A08-13** — Runtime bundles are Linux ELF/glibc artifacts, not generic host runtimes
- **A08-14** — CPU capability detection is duplicated and inconsistent
- **A08-15** — Recovery must not equate unavailable process observation with death
- **A08-16** — Cross-process registry locking disappears on non-Unix targets
- **A08-17** — Package path/executable and private-key semantics need platform-aware validation
- **A08-18** — Directory and installed-tool discovery is scattered
- **A08-19** — Greeter login/session discovery and power commands are Linux session providers
- **A08-20** — In-session unlock is a Linux PAM privileged helper
- **A08-21** — Installer inventory and install execution belong to Fedora Bootc deployment provider
- **A08-22** — NTFS broker/parser boundary must remain stronger than a generic file reader
- **A08-23** — NTFS native build and staging are explicit target tooling; vendor is not extraction fodder
- **A08-24** — Power governor daemon is a Linux image component

### [09-system](09-system.md)

- **A09-01** — Durable/private file replacement is already centralized, but exposes Unix semantics
- **A09-02** — Persistence locking, file identity and canonical-path ownership
- **A09-03** — User/config/cache/runtime directories and process identity leak into policy stores
- **A09-04** — Bounded child execution must keep process-tree cancellation semantics
- **A09-05** — Hardware/session action facade mixes portable contracts with Linux commands
- **A09-06** — Bluetooth and power already have strong separation seams
- **A09-07** — Media privacy and trusted client identity are Linux-specific security mechanisms
- **A09-08** — Gamepad forwarding has reusable semantic interfaces and explicit Linux backends
- **A09-09** — Tool launching and desktop application discovery are freedesktop integrations
- **A09-10** — Credential custody is Secret Service-specific with local-key fallback
- **A09-11** — Native TLS trust is deliberately fixed to the Tilo Fedora image
- **A09-12** — TTS factory's purported subprocess portability still depends on Unix process APIs
- **A09-13** — Windows appliance is a Linux host service, not a Windows-native host backend
- **A09-14** — Projector combines portable protocols with Linux discovery/capture process management
- **A09-15** — Image boot/rollback/extension tools should remain explicit Linux deployment backends
- **A09-16** — Tinker and optional swipe native build have explicit Linux/host-toolchain contracts

### [10-collab-identity](10-collab-identity.md)

- **A10-01** — secure identity persistence contains platform backends in the identity domain (high)
- **A10-02** — image trust and machine provisioning are explicitly Linux deployment backends (high boundary priority)
- **A10-03** — requester lifetime and ownership are Linux procfs concepts exposed by the relay (high)
- **A10-04** — polkit user resolution, service control, helper execution and ownership restoration belong in a privileged platform broker (high)
- **A10-05** — filesystem locations and daemon lifecycle are embedded in domain defaults (medium)
- **A10-06** — compute has a good sandbox trait but still embeds Linux filesystem mechanics outside it (high)
- **A10-07** — compute eligibility probes Linux power and launcher paths directly (medium)
- **A10-08** — enrollment LAN discovery relies on platform socket delivery semantics (medium)
- **A10-09** — collaboration persistence repeats Unix private-file and durable-replace mechanics (medium)
- **A10-10** — native codec dependency needs an intentional media backend boundary (low/architecture)
- **A10-11** — native tests are not consistently isolated to their actual backend (medium)

### [11-media-browser-text](11-media-browser-text.md)

- **A11-01** — PipeWire graph backend and identifiers leak into the shared audio surface
- **A11-02** — Audio diagnostics own unguarded Unix process machinery; accessibility policy recognizes a backend sink name
- **A11-03** — GStreamer playback is already a backend but the public engine exports its native controls
- **A11-04** — DMA-BUF is the only GPU frame handle and is guarded as Unix rather than Linux
- **A11-05** — Codec capabilities and tuning are GStreamer adapter data embedded in shared encode/decode policy
- **A11-06** — Event sound selection and native playback belong in platform adapters
- **A11-07** — PDFium discovery and process-global native lifetime are duplicated backend responsibilities
- **A11-08** — OCR preparation is portable; Tesseract execution is a replaceable native engine adapter
- **A11-09** — Spellcheck combines portable English engine with native Hunspell, POSIX locale and XDG persistence
- **A11-10** — Workspace mount resolution and process identity hints belong to platform/session context
- **A11-11** — Native verification must move with adapters, and successful early returns do not verify DMA-BUF

### [12-other-crates](12-other-crates.md)

- **A12-01** — Model gateway transport exposes Unix descriptors and Linux peer authentication
- **A12-02** — Local model provisioning already has a boundary, but daemon mixes hardware, Bootc and service management
- **A12-03** — Private model files and release signer need semantic secure-file operations
- **A12-04** — Format host's public transport module and configuration fallback leak local OS choices
- **A12-05** — Document blobstore chooses Linux-style cache root itself
- **A12-06** — Windows compatibility planning exposes the Linux capsule representation
- **A12-07** — Wine prefix store/DXVK deployment contain deliberate secure Unix filesystem implementations
- **A12-08** — Compatibility session authentication and fd-passing are Linux IPC
- **A12-09** — Compatibility audio, VM presentation and translated process lifecycle are native backend implementations
- **A12-10** — Developer tooling can stay host-specific, but its reusable library/build consumers inherit it

### [13-filesystem](13-filesystem.md)

- **A13-01** — FUSE dependency and native mount lifecycle must become an explicit backend (high)
- **A13-02** — Namespace and business operations expose FUSE/POSIX representations (high)
- **A13-03** — Multiple local protocols hard-code Unix sockets and Unix authorization (high)
- **A13-04** — Deployment paths, system identity selection and systemd activation belong at host composition (medium)
- **A13-05** — Predictive hydration probes Linux network/power and POSIX storage directly (high)
- **A13-06** — Private storage, file identity and durable directory flush need shared platform semantics (high)
- **A13-07** — Recipient mounting is coupled to spawning the filesystem CLI (medium)
- **A13-08** — Native integration tests need an explicit Linux test lane; current guards are insufficient (high)

### [14-os-tooling](14-os-tooling.md)

- **A14-01** — Control clients encode Unix transport and filesystem endpoint discovery throughout automation
- **A14-02** — Shared harness process-lifetime helper silently loses its Linux guarantee
- **A14-03** — Generic build/benchmark entry points carry host diagnostics and GNU tool assumptions
- **A14-04** — Fuzz harness manifest ties independent wire fuzzing to Linux sandbox dependencies
- **A14-05** — Privileged Linux service, hardware, session and appliance helpers are existing backend entry points
- **A14-06** — Fedora images, native packages and toolchain flags are intentionally target-specific
- **A14-07** — Native browser engine build wiring is application-owned and must follow backend extraction

### [15-vendor-site](15-vendor-site.md)

- **A15-01** — Destination is a proposed facade plus focused service crates, not an existing monolithic platform crate
- **A15-02** — Tilo-specific Wayland drag/drop includes storage policy inside the vendor backend
- **A15-03** — Gesture normalization is an intentional vendor platform boundary
- **A15-04** — Opt-in Metal presentation feedback must retain its asynchronous native contract
- **A15-05** — Site platform choices are release/provisioning contracts, not runtime detection to move into Rust
- **A15-06** — Media capture has intentional Linux host escape/tool discovery, outside app runtime
- **A15-07** — Portable renderer patches and compiler compatibility are false positives for OS extraction

Source anchors in the domain reports are navigation aids and must be rechecked
against current code before implementation. They support the reviewed finding;
they do not establish cross-platform runtime behavior on their own.
