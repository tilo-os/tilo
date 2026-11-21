# ---- Pinned Windows compatibility runtimes -------------------------------
# Proton is the runtime an x86 capsule starts for a program its owner chose to
# open with Proton, by starting Proton's own script on the system's libraries.
# The Steam Runtime is not shipped: its container starts a nested sandbox,
# which a capsule's Landlock refuses. An ARM computer translates Wine through
# FEX and plans no Proton route, so the ARM payload carries no Proton.
FROM ${BASE_REF} AS windows-compat-artifacts
## Bottles' upstream program records improve catalogue matching. They are data,
## never an instruction source: Tilo imports identity/runner hints only and does
## not execute a record's Steps, URLs, shell snippets, or dependencies.
## Proton names much of its content twice (the mono assemblies of each .NET API
## level, the prefix template's copies of wine's NLS files): hard-link those
## within the tree so its layer carries 80 MB once.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=os/scripts/hardlink-identical.py,target=/run/tilo-hardlink-identical.py,ro \
    mkdir -p /out/proton /out/catalogues \
    && install -D -m 0644 /tilo-assets/umu-database-8297d420.csv /out/catalogues/umu-database.csv \
    && if [ "$(uname -m)" = x86_64 ]; then \
         tar -xzf /tilo-assets/umu-proton-10.0-4.tar.gz -C /out/proton \
         && test -x /out/proton/UMU-Proton-10.0-4/proton \
         && python3 /run/tilo-hardlink-identical.py /out/proton; \
       else \
         test "$(uname -m)" = aarch64; \
       fi \
    && mkdir -p /out/catalogues/bottles-programs \
    && tar -xzf /tilo-assets/bottles-programs-d1160b8.tar.gz --strip-components=1 -C /out/catalogues/bottles-programs \
    && chmod -R a-w /out/catalogues/bottles-programs
