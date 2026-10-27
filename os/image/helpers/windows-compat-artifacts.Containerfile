# ---- Pinned Windows compatibility runtimes -------------------------------
# UMU is the upstream-supported way to run Proton outside Steam. Keep the
# launcher and the default Proton build as independently pinned artifacts:
# catalogues may select another installed runner, while a fresh image is still
# able to provision an offline game capsule. Ship the matching Steam runtime
# as immutable content; UMU metadata, runtime locks, and variable state belong
# to each capsule. See os/docs/proton-runtime-validation.md for that contract.
FROM ${BASE_REF} AS windows-compat-artifacts
## Bottles' upstream program records improve catalogue matching. They are data,
## never an instruction source: Tilo imports identity/runner hints only and does
## not execute a record's Steps, URLs, shell snippets, or dependencies.
## Proton names much of its content twice (the mono assemblies of each .NET API
## level, the prefix template's copies of wine's NLS files): hard-link those
## within the tree so its layer carries 80 MB once. The Steam runtime
## is left as pv-verify checked it.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=os/scripts/hardlink-identical.py,target=/run/tilo-hardlink-identical.py,ro \
    mkdir -p /out/bin /out/proton /out/steam-runtime \
    && install -D -m 0644 /tilo-assets/umu-database-8297d420.csv /out/catalogues/umu-database.csv \
    && tar -xf /tilo-assets/umu-launcher-1.4.0.tar -C /tmp \
    && install -m 0755 /tmp/umu/umu-run /out/bin/umu-run \
    && install -m 0644 /tmp/umu/umu_run.py /out/bin/umu_run.py \
    && tar -xzf /tilo-assets/umu-proton-10.0-4.tar.gz -C /out/proton \
    && test -x /out/proton/UMU-Proton-10.0-4/proton \
    && python3 /run/tilo-hardlink-identical.py /out/proton \
    && tar -xJf /tilo-assets/steam-runtime-sniper-3.0.20260805.254768.tar.xz -C /out/steam-runtime \
    && case "$(uname -m)" in \
         x86_64) pv=pressure-vessel ;; \
         aarch64) pv=pressure-vessel-arm64 ;; \
         *) echo "unsupported Steam runtime host architecture: $(uname -m)" >&2; exit 1 ;; \
       esac \
    && /out/steam-runtime/SteamLinuxRuntime_sniper/${pv}/bin/pv-verify /out/steam-runtime/SteamLinuxRuntime_sniper \
    && touch /out/steam-runtime/SteamLinuxRuntime_sniper/sniper_platform_3.0.20260805.254768/files/.ref \
    && mkdir -p /out/catalogues/bottles-programs \
    && tar -xzf /tilo-assets/bottles-programs-d1160b8.tar.gz --strip-components=1 -C /out/catalogues/bottles-programs \
    && chmod -R a-w /out/catalogues/bottles-programs
