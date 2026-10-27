# The browser engine runtime as the registry package that offers it
# (`so.tilo.runtime.chromium`), for a publication to submit: the thin package
# and its content-addressed payload objects, plus the packaging CLI that
# submits them. Built only when that target is asked for; the image never
# contains it.
FROM apps-payload AS engine-runtime-package-build
ARG CHROMIUM_RUNTIME_VERSION
ARG ENGINE_RUNTIME_PACKAGE_VERSION=0.1.0
RUN --mount=type=secret,id=tapp_publisher_identity \
    --mount=type=bind,source=tools/apps/pack-runtime-package.sh,target=/usr/local/libexec/tilo-pack-runtime-package.sh,ro \
    test -s /run/secrets/tapp_publisher_identity \
    && test -f "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}/bundle.toml" \
    && bash /usr/local/libexec/tilo-pack-runtime-package.sh \
         "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}" so.tilo.runtime.chromium \
         "Browser engine" "${ENGINE_RUNTIME_PACKAGE_VERSION}" \
         /run/secrets/tapp_publisher_identity /out /engine-runtime-package

FROM scratch AS engine-runtime-package
COPY --from=engine-runtime-package-build /engine-runtime-package/ /package/
COPY --from=engine-runtime-package-build /out/tilo-app-package /bin/tilo-app-package
