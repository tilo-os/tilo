# Vendored MLS

DAVE is Discord's end-to-end encryption for calls (https://daveprotocol.com). It is MLS
(RFC 9420), ciphersuite 2 (`MLS_128_DHKEMP256_AES128GCM_SHA256_P256`), and the bridge's program
needs it in bun's interpreter. These are the packages, copied whole, with no native code:

| package       | version | tarball integrity                                          |
| ------------- | ------- | ---------------------------------------------------------- |
| `ts-mls`      | 1.6.4   | `sha512-BFb9qJ3V1+HIuifC+MtnlROEj3oc6NEzA7Ig1/LAmANDRhNay7UCnwkzijzHH9cav5wf1BoJw8KQ2mXiEhPy1Q==` |
| `@hpke/core`  | 1.9.0   | `sha512-pFxWl1nNJeQCSUFs7+GAblHvXBCjn9EPN65vdKlYQil2aURaRxfGMO6vBKGqm1YHTKwiAxJQNEI70PbSowMP9Q==` |
| `@hpke/common` | 1.10.0 | `sha512-uVq9pTNERQ1GcFlHZzQx+a0ZMC81wQzkbNzJPEyR/l3AWM7fASd/qYN2Cnq6uL1NPEfwcD4lgOmfjjZfx2k2XA==` |

`ts-mls` is MIT (LICENSE-ts-mls); `@hpke` is MIT (LICENSE-hpke-core, LICENSE-hpke-common).
Only the `.js` files are copied; source maps and declarations are dropped. `hpke/core` is
`@hpke/core`'s `esm/` directory at `hpke/core/`, and `hpke/common` is `@hpke/common`'s `esm/`
at `hpke/common/`.

## What the copy changes

- Bare imports are rewritten to relative paths (`@hpke/core` → `hpke/core/mod.js`,
  `@hpke/common` → `hpke/common/mod.js`), so the app runs from the package tree with no
  `node_modules` and no install step.
- `ts-mls/externalSender.js` writes and reads the RFC 9420 form of the `external_senders`
  group context extension, `ExternalSender external_senders<V>` (a vector), instead of one
  extension per sender written bare. DAVE's group context carries the voice gateway there.
- `hpke/common/src/kdfs/hkdf.js` leaves the key length out of the WebCrypto HMAC algorithm.
  bun refuses to import a raw HMAC key whose byte length differs from a `length` given in
  bits there, which the media key ratchet's 16-byte secrets hit.

## The crypto that runs

MLS hashing, KDF, HPKE P-256 and AEAD are WebCrypto (`crypto.subtle`). The P-256 signature
provider is the bridge's own (`apps/tl/discord/src/dave.js`), also WebCrypto, so no CPU-bound
elliptic curve code runs in the interpreter; the optional `@noble/*` imports in ts-mls are
never reached. The only supported ciphersuite is the one DAVE names; the other suites'
optional dependencies are not vendored.
