FROM --platform=linux/arm64 ${FEX_PAYLOAD_REF} AS windows-fex-input
FROM --platform=linux/amd64 ${WINDOWS_GUEST_REF} AS windows-guest-input

FROM ${BASE_REF} AS windows-provider-aarch64-1
ARG FEX_PAYLOAD_REF
ARG WINDOWS_GUEST_REF
COPY os/dev/windows_provider.py os/dev/validate-fex-payload.py os/dev/validate-windows-guest.py os/dev/apply-fex-patches.py /tilo/
COPY os/external-assets/fex-source.env os/dev/wine-source.json /tilo/
COPY os/external-assets/fex-patches/ /tilo/fex-patches/
RUN --network=none \
    --mount=type=bind,from=windows-fex-input,source=/,target=/fex,ro \
    --mount=type=bind,from=windows-guest-input,source=/,target=/guest,ro \
    test "$(uname -m)" = aarch64 \
    && mkdir -p /out/usr/share/tilo/windows /payload/usr/lib/tilo/windows-runners \
    && python3 /tilo/windows_provider.py validate \
      --fex-ref "$FEX_PAYLOAD_REF" --guest-ref "$WINDOWS_GUEST_REF" \
      --fex-root /fex --guest-root /guest/rootfs \
      --source-pin /tilo/fex-source.env --wine-pin /tilo/wine-source.json \
      > /out/usr/share/tilo/windows/provider.json \
    && cp -a /fex/usr/. /payload/usr/ \
    && cp -a /guest/rootfs /payload/usr/lib/tilo/windows-runners/x86

FROM ${BASE_REF} AS windows-provider-x86_64-1
COPY os/dev/windows_provider.py /tilo/
RUN --network=none test "$(uname -m)" = x86_64 \
    && mkdir -p /out/usr/share/tilo/windows /payload \
    && python3 /tilo/windows_provider.py inputs --arch x86_64 \
       > /out/usr/share/tilo/windows/provider.json

# Without the provider only its build record is copied. `absent` rejects any
# architecture but aarch64, so an x86_64 build selecting this fails here.
FROM ${BASE_REF} AS windows-provider-off
COPY os/dev/windows_provider.py /tilo/
RUN --network=none mkdir -p /out/usr/share/tilo/windows /payload \
    && python3 /tilo/windows_provider.py absent --arch "$(uname -m)" \
       > /out/usr/share/tilo/windows/provider.json
FROM windows-provider-off AS windows-provider-aarch64-0
FROM windows-provider-off AS windows-provider-x86_64-0

FROM windows-provider-${TILO_WINDOWS_ARCH}-${TILO_WITH_WINDOWS} AS windows-provider
