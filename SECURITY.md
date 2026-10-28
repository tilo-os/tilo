# Security policy

## Reporting a vulnerability

Email **security@tilo.so**. Encrypt sensitive details to our OpenPGP key,
fingerprint `4F09 6905 6857 A7A8 54B6  3150 8184 3672 9F72 CC0A`, published at
<https://tilo.so/.well-known/security.asc> and on keys.openpgp.org.

Do not open a public issue containing exploit details, private keys, identity
records, invite tokens or user data. Include the affected commit or image
digest, configuration, impact, reproduction steps and whether you suspect
active exploitation.

## What to expect

- We acknowledge your report within 3 business days.
- We give an initial assessment within 7 days.
- We agree a disclosure date with you, 90 days after the report by default.
  We may ask for more time for a fix that needs coordination elsewhere, or
  disclose sooner once a fix has shipped.
- We credit you in the advisory unless you prefer not to be named.

## Supported versions

Tilo is pre-release software. Fixes land on `main` and in the next published
image; earlier images and branches are not patched.

## Scope

Everything in this repository and the services we run on tilo.so, tilo.sh and
tilo.coffee is in scope. Vulnerabilities in third-party software we ship or
run belong with that project; tell us too if Tilo is affected. Denial of
service by volume, social engineering and physical attacks on our
infrastructure are out of scope.

## Safe harbor

We will not take legal action against, or ask law enforcement to investigate,
research that:

- makes a good-faith effort to avoid privacy violations, data destruction and
  service disruption;
- only accesses data needed to demonstrate the vulnerability, and does not
  access, modify or keep other people's data;
- uses accounts and workspaces you own or have explicit permission to test;
- reports the vulnerability to us promptly and keeps it confidential until the
  agreed disclosure date.

If a third party takes legal action over research that followed this policy,
we will make clear that it was authorised.

## Handling reports

Maintainers acknowledge a report, preserve evidence, assign an owner and
severity, and coordinate a fix and disclosure date with the reporter. A release
key, identity root, rollback, sandbox escape, cross-workspace authorization, or
verified-boot bypass is presumptively critical until triaged. Rotate or revoke
affected trust roots, publish signed remediation guidance, and do not rely on
silent image replacement: fixed artifacts receive a new dense generation.

The current assurance boundary and known limitations are documented in
[docs/security/threat-model.md](docs/security/threat-model.md). Reports that
demonstrate a documented limitation are still useful when they improve the
exploitability assessment or a mitigation.
