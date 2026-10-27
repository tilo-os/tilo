# Public trust parsing is a build-only operation. The pinned base supplies the
# OpenSSL CLI while its ABI libraries remain part of the runtime package policy.
FROM ${BASE_REF} AS public-trust-tools
