# Target policy signing fixture

These files are a real RSA PKCS#1 v1.5 SHA-256 signed target bundle for
`apps/imager/packaging/test_target_policy.py`. The authority is a throwaway test
key; it is **not** a Tilo release identity and the private key is deliberately
not committed. The fixture proves archive shape, generation/architecture
binding, and signature rejection on changed bytes.

The PE payload and its detached signature are stored as ASCII hexadecimal so
the repository stays text-only. The test decodes them into a private temporary
directory under `$TMPDIR` before handing the exact three bundle files to the
producer. The encoded files are sources, never bundle members.

- `authority.pem`: the test authority's RSA public key.
- `encoded/tilo-target.efi.hex`: a minimal x86_64 PE image with ordinary fixture
  bytes, as hexadecimal.
- `predictions/generation-7.json`: the signed SHA-256 PCR 11 prediction over
  `approved_policy || be64(7)`.
- `encoded/predictions/generation-7.json.uki.sig.hex`: the detached signature
  over the decoded `tilo-target.efi` bytes, as hexadecimal.
- `image-config-{x86_64,aarch64}.json`: the image-config shape `skopeo inspect
  --config` returns, with the source revision label at `a` repeated 40 times.

To regenerate, use a disposable key outside the checkout. Do not add the
private key to the repository. Write the PE image and signature outside the
checkout, then record only their hexadecimal form in the fixture.

```sh
work="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-target-fixture.XXXXXX")"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$work/key.pem"
openssl pkey -in "$work/key.pem" -pubout -out authority.pem
# Write "$work/tilo-target.efi", then:
openssl dgst -sha256 -sign "$work/key.pem" \
  -out "$work/generation-7.json.uki.sig" "$work/tilo-target.efi"
for source in tilo-target.efi predictions/generation-7.json.uki.sig; do
  mkdir -p "encoded/$(dirname "$source")"
  od -An -v -tx1 "$work/$source" | sed 's/^ *//' > "encoded/$source.hex"
done
# Sign approved_policy || big-endian uint64(7) with the same key and put the
# standard-base64 approved policy and signature into generation-7.json.
```

The producer and its test use `os/dev/probe-release-signer.py`'s standard
library RSA verification, which is the same check the release-signer probe
performs against `keys/tpm2-pcr-signing.public.pem` in production.
