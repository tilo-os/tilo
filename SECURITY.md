# Security policy

Please report suspected vulnerabilities privately to the qOS maintainers using
the repository host's private security-advisory channel. Do not open a public
issue containing exploit details, private keys, identity records, invite tokens,
or user data. Include the affected commit/image digest, configuration, impact,
reproduction steps and whether active exploitation is suspected.

Maintainers should acknowledge a report, preserve evidence, assign an owner and
severity, and coordinate a fix and disclosure date with the reporter. A release
key, identity root, rollback, sandbox escape, cross-workspace authorization, or
verified-boot bypass is presumptively critical until triaged. Rotate or revoke
affected trust roots, publish signed remediation guidance, and do not rely on
silent image replacement: fixed artifacts receive a new dense generation.

The current assurance boundary and known limitations are documented in
[docs/security/threat-model.md](docs/security/threat-model.md). Reports that
demonstrate a documented limitation are still useful when they improve the
exploitability assessment or a mitigation.
