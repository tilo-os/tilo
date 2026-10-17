{ lib
, rustPlatform
, fetchurl
, runCommand
, pkg-config
, cmake
, clang
, capnproto
, buildPackages
, makeWrapper
, bubblewrap
, xdg-dbus-proxy
, xwayland-satellite
, pam
, libxkbcommon
, wayland
, seatd
, libinput
, systemd
, libdrm
, libgbm
, mesa
, pipewire
, dbus
, fontconfig
, freetype
, harfbuzz
, fuse3
, gst_all_1
}:

let
  repoRoot = toString ../..;
  # The fonts crate's build script fetches its pinned catalog by URL and SHA-256 into a cache
  # (tools/font-catalog.py). The Nix sandbox has no network, so the same artifacts arrive as
  # fixed-output fetches, laid out as that cache: <sha256>/<file>, regular files. The script
  # also writes its prepared catalogs into the cache, so the build copies it somewhere writable.
  fontArtifacts = builtins.fromJSON
    (builtins.readFile ../../crates/sdk/ui-core/assets/fonts/artifacts.json);
  fontAssetCache = runCommand "tilo-font-artifacts" { } (lib.concatMapStrings (font: ''
    install -Dm644 ${fetchurl { inherit (font) url sha256; }} "$out/${font.sha256}/${font.file}"
  '') fontArtifacts.assets);
in
rustPlatform.buildRustPackage rec {
  pname = "tilo-session";
  version = lib.removeSuffix "\n" (builtins.readFile ../VERSION);
  src = lib.cleanSourceWith {
    src = ../..;
    filter = path: type:
      lib.cleanSourceFilter path type
      && !(type == "directory" && builtins.elem (baseNameOf path) [ "target" "result" ])
      # Nix and CI metadata must not change the Rust source derivation.
      && !(builtins.elem (toString path) [
        "${repoRoot}/flake.nix"
        "${repoRoot}/flake.lock"
        "${repoRoot}/dist/nix"
        "${repoRoot}/.forgejo"
        "${repoRoot}/.codex"
      ]);
  };

  # The lock file selects registry crates directly. The three pinned Git
  # repositories can be fetched during evaluation without hand-maintained
  # output hashes outside nixpkgs.
  cargoLock = {
    lockFile = ../../Cargo.lock;
    allowBuiltinFetchGit = true;
  };
  cargoBuildFlags = [
    "-p" "launcher"
    "-p" "tilo-unlock-helper"
    "-p" "tilo-session-record"
    "-p" "shell"
    "-p" "imager"
    "--bin" "tilo-launcher"
    "--bin" "tilo-unlock-helper"
    "--bin" "tilo-flight-recorder"
    "--bin" "tilo-session-report"
    "--bin" "shell"
    "--bin" "imager"
    "--no-default-features"
  ];
  cargoTestFlags = cargoBuildFlags;
  doCheck = false;

  # The shell is a large monolithic binary. Nix builds run in a sanitized
  # derivation environment, so the package-container profile overrides do not
  # cross this boundary. Keep release optimisation while avoiding the
  # multi-gigabyte fat-LTO rustc peak that exhausts the shared package runner.
  CARGO_PROFILE_RELEASE_LTO = "thin";
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16";

  # Build scripts run python3 on the build machine (font catalog, NTFS helper).
  nativeBuildInputs = [ pkg-config cmake clang capnproto makeWrapper python3 ];
  buildInputs = [
    libxkbcommon wayland seatd libinput systemd libdrm libgbm mesa pipewire dbus
    fontconfig freetype harfbuzz fuse3
    gst_all_1.gstreamer gst_all_1.gst-plugins-base
  ];
  # Bindgen executes on the build machine even when the package's host
  # platform is ARM, so it must load the build-platform libclang.
  LIBCLANG_PATH = "${lib.getLib buildPackages.libclang}/lib";
  # The FHS packages use /usr/libexec. Nix must seal the immutable store path
  # into the compositor or locking the session could never invoke PAM.
  preBuild = ''
    export TILO_DESKTOP_TRUSTED_SHELL_PATH="$out/bin/.tilo-shell-wrapped"
    export TILO_UNLOCK_HELPER_PATH="$out/libexec/tilo-unlock-helper"
    export TILO_FONT_ASSET_CACHE="$TMPDIR/font-artifacts"
    cp -r --no-preserve=mode ${fontAssetCache} "$TILO_FONT_ASSET_CACHE"
  '';

  installPhase = ''
    runHook preInstall
    shell_bin=$(find target -type f -path '*/release/shell' -perm -0100 -print -quit)
    test -n "$shell_bin"
    bash "$src/dist/stage-payload.sh" "$(dirname "$shell_bin")" "$TMPDIR/tilo-payload"
    p="$TMPDIR/tilo-payload"
    install -Dm755 "$p/bin/tilo-shell" "$out/bin/tilo-shell"
    install -Dm755 "$p/bin/tilo-launcher" "$out/bin/tilo-launcher"
    install -Dm755 "$p/libexec/tilo-unlock-helper" "$out/libexec/tilo-unlock-helper"
    install -Dm755 "$p/libexec/tilo-session" "$out/libexec/tilo-session"
    install -Dm755 "$p/libexec/tilo-session-runner" "$out/libexec/tilo-session-runner"
    install -Dm755 "$p/libexec/tilo-flight-recorder" "$out/libexec/tilo-flight-recorder"
    install -Dm755 "$p/libexec/tilo-session-report" "$out/libexec/tilo-session-report"
    install -Dm644 "$p/pam/tilo-unlock" "$out/etc/pam.d/tilo-unlock"
    install -Dm644 "$p/wayland-sessions/tilo.desktop" "$out/share/wayland-sessions/tilo.desktop"
    install -Dm644 "$p/portal/tilo.portal" "$out/share/xdg-desktop-portal/portals/tilo.portal"
    install -Dm644 "$p/portal/tilo-portals.conf" "$out/share/xdg-desktop-portal/tilo-portals.conf"
    install -Dm644 "$p/pipewire/tilo-echo-cancel.conf" "$out/share/pipewire/tilo-echo-cancel.conf"
    install -Dm644 "$p/systemd-user/tilo-graphical-session.target" "$out/share/systemd/user/tilo-graphical-session.target"
    install -Dm644 "$p/license/LICENSE" "$out/share/licenses/tilo-session/LICENSE"
    install -Dm755 "$p/bin/tilo-import-folders" "$out/bin/tilo-import-folders"
    install -Dm644 "$p/applications/tilo-import-folders.desktop" "$out/share/applications/tilo-import-folders.desktop"
    substituteInPlace "$out/share/applications/tilo-import-folders.desktop" --replace-fail "/usr/bin/tilo-import-folders" "$out/bin/tilo-import-folders"

    # Store paths replace FHS paths in the wrapper and session entry.
    substituteInPlace "$out/libexec/tilo-session" \
      --replace-fail 'TILO_SHELL=/usr/bin/tilo-shell' "TILO_SHELL=$out/bin/tilo-shell" \
      --replace-fail 'SESSION_RUNNER=/usr/libexec/tilo-session-runner' "SESSION_RUNNER=$out/libexec/tilo-session-runner"
    substituteInPlace "$out/libexec/tilo-session-runner" \
      --replace-fail 'SYSTEMCTL=/usr/bin/systemctl' "SYSTEMCTL=${systemd}/bin/systemctl" \
      --replace-fail 'SYSTEMD_CAT=/usr/bin/systemd-cat' "SYSTEMD_CAT=${systemd}/bin/systemd-cat" \
      --replace-fail 'DBUS_UPDATE=/usr/bin/dbus-update-activation-environment' "DBUS_UPDATE=${dbus}/bin/dbus-update-activation-environment" \
      --replace-fail 'FLIGHT_RECORDER=/usr/libexec/tilo-flight-recorder' "FLIGHT_RECORDER=$out/libexec/tilo-flight-recorder"
    substituteInPlace "$out/share/wayland-sessions/tilo.desktop" \
      --replace-fail 'Exec=/usr/libexec/tilo-session' "Exec=$out/libexec/tilo-session" \
      --replace-fail 'TryExec=/usr/bin/tilo-shell' "TryExec=$out/bin/tilo-shell"
    grep -a -F "$out/libexec/tilo-unlock-helper" "$out/bin/tilo-shell"
    runHook postInstall
  '';

  postFixup = ''
    # A NixOS session has no FHS-wide PATH or ldconfig cache. Seal the runtime
    # tools and PAM library into wrappers so both direct package installs and
    # the NixOS module produce a usable session.
    wrapProgram "$out/bin/tilo-shell" \
      --prefix PATH : ${lib.makeBinPath [ bubblewrap xdg-dbus-proxy xwayland-satellite ]}
    wrapProgram "$out/bin/tilo-launcher" \
      --prefix PATH : ${lib.makeBinPath [ bubblewrap xdg-dbus-proxy ]}
    wrapProgram "$out/libexec/tilo-unlock-helper" \
      --set TILO_PAM_LIB ${lib.getLib pam}/lib/libpam.so.0
    grep -F '${bubblewrap}/bin' "$out/bin/tilo-shell"
    grep -F '${xwayland-satellite}/bin' "$out/bin/tilo-shell"
    grep -F '${xdg-dbus-proxy}/bin' "$out/bin/tilo-launcher"
    grep -F '${lib.getLib pam}/lib/libpam.so.0' "$out/libexec/tilo-unlock-helper"
  '';

  # NixOS display managers require this when the module registers the package
  # in services.displayManager.sessionPackages.
  passthru.providedSessions = [ "tilo" ];

  meta = {
    description = "Tilo collaborative compositor as a Wayland session";
    homepage = "https://code.tilo.so/tilo/tilo";
    license = lib.licenses.agpl3Only;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
    mainProgram = "tilo-shell";
  };
}
