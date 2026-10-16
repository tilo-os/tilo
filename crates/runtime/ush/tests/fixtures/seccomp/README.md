# Native seccomp program references

These required x86_64 golden programs pin the production policy byte for byte.
Their LF hex representation contains one serialized eight-byte `sock_filter`
instruction per line, in the original little-endian byte order. The tests
independently decode these fixed bytes and compare them against the production
compiler; no policy is generated from the implementation under test.

The bytes are unchanged from the original fixtures at `2a26be496c` and retain
the repository's license. No network, asset cache or extra generation tool is
needed to compile or run these tests. Malformed hex fails the test.

Original byte pins:

- `ush.x86_64.bpf`: 616 bytes, SHA-256 `4a92b0cc628da71cb3c392a5f2ed23233943a48514302de09954a39d51d5f33b`.

A deliberate policy change still requires review of the regenerated program
and its expected text bytes. Never replace the golden with a call to the
current compiler: that would erase the independent policy comparison.
