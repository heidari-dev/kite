# Security policy

Kite is in early development and has no supported releases yet.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting:
https://github.com/heidari-dev/kite/security/advisories/new

Include what you found, how to reproduce it, and the affected version or commit. You should get a first
reply within a week. Once a fix is released we will credit you unless you prefer otherwise.

## Scope

Of most interest: the encrypted session and pairing, the phone-side relay (which opens sockets on
behalf of the computer), and anything that could leak traffic or DNS outside the phone's VPN.
