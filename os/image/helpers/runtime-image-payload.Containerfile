# The runtimes travel with their publishers' keys from the one builder result:
# a non-publishing build signs them with keys it generates, so a key taken from
# another stage (native-builder-payload, which buildkit may serve from an
# earlier invocation's cache while the builder re-runs) would not be theirs.
FROM ${BASE_REF} AS runtime-image-payload
RUN --network=none \
    --mount=type=bind,from=apps-payload,source=/usr/lib/tilo-runtime,target=/tilo-runtime,ro \
    --mount=type=bind,from=apps-payload,source=/out/runtime-version,target=/tilo-runtime-version,ro \
    --mount=type=bind,from=apps-payload,source=/out/runtime-bundle-publisher.hex,target=/tilo-runtime-publisher.hex,ro \
    --mount=type=bind,from=apps-payload,source=/out/browser-runtime-publisher.hex,target=/tilo-browser-runtime-publisher.hex,ro \
    --mount=type=bind,from=apps-payload,source=/out/language-packs,target=/tilo-language-packs,ro \
    --mount=type=bind,from=os-fonts,source=/out/fonts,target=/tilo-os-fonts,ro \
    --mount=type=bind,source=tools/font-catalog.py,target=/tilo-src/tools/font-catalog.py,ro \
    --mount=type=bind,source=crates/sdk/ui-core/assets/fonts,target=/tilo-src/crates/sdk/ui-core/assets/fonts,ro \
    --mount=type=bind,source=os/scripts/hardlink-identical.py,target=/tilo-src/hardlink-identical.py,ro \
    mkdir -p /image/usr/lib /image/usr/share/tilo/font-catalogs /image/usr/share/fonts \
    && cp -a /tilo-runtime /image/usr/lib/tilo-runtime \
    && cp /tilo-runtime-publisher.hex /image/usr/share/tilo/runtime-bundle-publisher.hex \
    && cp /tilo-browser-runtime-publisher.hex /image/usr/share/tilo/browser-runtime-publisher.hex \
    && cp -a /tilo-language-packs /image/usr/share/tilo/language-packs \
    && fonts="/image/usr/lib/tilo-runtime/$(cat /tilo-runtime-version)/lib/fonts" \
    && catalog="$(sha256sum "$fonts/manifest.toml" | cut -d ' ' -f1)" \
    && python3 /tilo-src/tools/font-catalog.py link "$fonts" \
        "/image/usr/share/tilo/font-catalogs/$catalog" --scope product \
    && python3 /tilo-src/tools/font-catalog.py link "$fonts" /image/usr/share/fonts/tilo \
        --scope os --extra /tilo-os-fonts \
    # The runtimes also share files (the browser runtime's loader set is the
    # same glibc the native runtime signs at lib/): one inode each.
    && python3 /tilo-src/hardlink-identical.py /image/usr \
    && test "$(stat -c %i "$fonts/NotoSansCJK-VF.ttc")" = "$(stat -c %i /image/usr/share/fonts/tilo/NotoSansCJK-VF.ttc)" \
    && test "$(stat -c %i "$fonts/NotoSansCJK-VF.ttc")" = "$(stat -c %i "/image/usr/share/tilo/font-catalogs/$catalog/NotoSansCJK-VF.ttc")"
