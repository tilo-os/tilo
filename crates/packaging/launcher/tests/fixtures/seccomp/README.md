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

- `tilo-default.x86_64.bpf`: 2,120 bytes, SHA-256 `030f7b61d28ae7732cd5cefa29e0f94d8c3888aa8effdb6f39b874f654a306e0`.
- `tilo-jit.x86_64.bpf`: 1,928 bytes, SHA-256 `12620bc3f7e60cde14bb1a9518022783c7e95ce1f2fb7073e45cb581a6e7a3b7`.
- `tilo-web-engine.x86_64.bpf`: 2,008 bytes, SHA-256 `6abc4e7b8d0bb8ea17e234af28f06e1c0a508e8f57d86474e11af79bf176b488`.

A deliberate policy change still requires review of the regenerated program
and its expected text bytes. Never replace the golden with a call to the
current compiler: that would erase the independent policy comparison.
