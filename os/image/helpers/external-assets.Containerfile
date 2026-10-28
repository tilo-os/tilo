# CI restores these checksum-pinned files from persistent storage before the
# build; local builds use the same resumable verifier.
FROM scratch AS external-assets
COPY --from=github-assets / /
