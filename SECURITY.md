# Security policy

Kite is in early development and has no supported releases yet.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting:
https://github.com/heidari-dev/kite/security/advisories/new

Include what you found, how to reproduce it, and the affected version or commit.

## Disclosure timeline

- Within 7 days: we confirm we got the report and tell you whether we can reproduce it.
- Within 90 days of the report: a fixed release is out. If a fix needs longer, we agree on a new date
  with you before the 90 days run out.
- When the fix ships: we publish a GitHub security advisory (with a CVE where one applies) describing
  the vulnerability, the affected versions and how to upgrade.
- We credit you in the advisory unless you prefer otherwise. Please keep the details private until
  the advisory is out.

## Scope

Of most interest: the encrypted session and pairing, the phone-side relay (which opens sockets on
behalf of the computer), and anything that could leak traffic or DNS outside the phone's VPN.

The [threat model](docs/threat-model.md) lists what we protect and what is still planned.
