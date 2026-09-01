{ lib
, rustPlatform
, pkg-config
, cmake
, clang
, capnproto
, buildPackages
, libxkbcommon
, wayland
, seatd
, libinput
, systemd
, libdrm
, mesa
, pipewire
, dbus
, openssl
, fontconfig
, freetype
, harfbuzz
, fuse3
, gst_all_1
}:

rustPlatform.buildRustPackage rec {
  pname = "qos-session";
  version = lib.removeSuffix "\n" (builtins.readFile ../VERSION);
  src = lib.cleanSourceWith {
    src = ../..;
    filter = path: type:
      lib.cleanSourceFilter path type
      && !(type == "directory" && builtins.elem (baseNameOf path) [ "target" "result" ]);
  };

  cargoLock.lockFile = ../../Cargo.lock;
  cargoBuildFlags = [
    "-p" "launcher"
    "-p" "qos-unlock-helper"
    "-p" "shell"
    "--no-default-features"
  ];
  cargoTestFlags = cargoBuildFlags;
  doCheck = false;

  nativeBuildInputs = [ pkg-config cmake clang capnproto ];
  buildInputs = [
    libxkbcommon wayland seatd libinput systemd libdrm mesa pipewire dbus
    openssl fontconfig freetype harfbuzz fuse3
    gst_all_1.gstreamer gst_all_1.gst-plugins-base
  ];
  # Bindgen executes on the build machine even when the package's host
  # platform is ARM, so it must load the build-platform libclang.
  LIBCLANG_PATH = "${lib.getLib buildPackages.libclang}/lib";
  # The FHS packages use /usr/libexec. Nix must seal the immutable store path
  # into the compositor or locking the session could never invoke PAM.
  preBuild = ''
    export QOS_UNLOCK_HELPER_PATH="$out/libexec/qos-unlock-helper"
  '';

  installPhase = ''
    runHook preInstall
    shell_bin=$(find target -type f -path '*/release/shell' -perm -0100 -print -quit)
    test -n "$shell_bin"
    "$src/dist/stage-payload.sh" "$(dirname "$shell_bin")" "$TMPDIR/qos-payload"
    p="$TMPDIR/qos-payload"
    install -Dm755 "$p/bin/qos-shell" "$out/bin/qos-shell"
    install -Dm755 "$p/bin/qos-launcher" "$out/bin/qos-launcher"
    install -Dm755 "$p/libexec/qos-unlock-helper" "$out/libexec/qos-unlock-helper"
    install -Dm755 "$p/libexec/qos-session" "$out/libexec/qos-session"
    install -Dm644 "$p/pam/qos-unlock" "$out/etc/pam.d/qos-unlock"
    install -Dm644 "$p/wayland-sessions/qos.desktop" "$out/share/wayland-sessions/qos.desktop"
    install -Dm644 "$p/portal/qos.portal" "$out/share/xdg-desktop-portal/portals/qos.portal"
    install -Dm644 "$p/license/LICENSE" "$out/share/licenses/qos-session/LICENSE"

    # Store paths replace FHS paths in the wrapper and session entry.
    substituteInPlace "$out/libexec/qos-session" \
      --replace-fail 'QOS_SHELL=/usr/bin/qos-shell' "QOS_SHELL=$out/bin/qos-shell"
    substituteInPlace "$out/share/wayland-sessions/qos.desktop" \
      --replace-fail 'Exec=/usr/libexec/qos-session' "Exec=$out/libexec/qos-session" \
      --replace-fail 'TryExec=/usr/bin/qos-shell' "TryExec=$out/bin/qos-shell"
    grep -a -F "$out/libexec/qos-unlock-helper" "$out/bin/qos-shell"
    runHook postInstall
  '';

  meta = {
    description = "qOS collaborative compositor as a Wayland session";
    homepage = "https://code.podesta.ai/qos/qos";
    license = lib.licenses.agpl3Only;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
    mainProgram = "qos-shell";
  };
}
