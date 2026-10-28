# Fedora DNF/RPM record wall-clock transaction times in rpmdb and history.
# Build the tiny clock shim separately so it does not invalidate Rust output.
FROM builder-base AS dnf-build-clock
COPY os/scripts/build-clock.c /tmp/build-clock.c
RUN cc -std=c11 -Wall -Wextra -Werror -O2 -fPIC -shared \
    /tmp/build-clock.c -o /build-clock.so
