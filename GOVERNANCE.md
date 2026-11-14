# Governance

Kite has one maintainer today. This page says who does what, how decisions are made, and what
happens if the maintainer is gone. How to contribute is in [CONTRIBUTING.md](CONTRIBUTING.md), and
the rules for behaviour are in the [Code of Conduct](CODE_OF_CONDUCT.md).

## Roles

**Maintainer** (today: [heidari-dev](https://github.com/heidari-dev), the repository owner)

- Triages issues, reviews and merges pull requests, and keeps `main` green.
- Decides what goes into a release, tags it, and publishes the draft that CI builds
  ([docs/releasing.md](docs/releasing.md)).
- Handles security reports ([SECURITY.md](SECURITY.md)) and Code of Conduct reports.
- Holds the admin rights on the repository and the Android signing key.

**Contributor**: anyone who opens an issue or discussion, comments, or sends a pull request. There is
no sign-up and no special access. A contributor follows CONTRIBUTING.md and the Code of Conduct,
and their work is licensed under GPL-3.0-or-later.

## How decisions are made

- **Everyday changes** (bug fixes, docs, small features): discussed in the issue or pull request.
  The maintainer merges when CI is green and the change is small and single-purpose. Silence is
  consent for a change nobody objected to.
- **Larger changes** (the wire protocol, crypto or pairing, a new platform, a new dependency, the
  licence, anything that widens what the phone relay may reach): proposed in an issue first and open
  for comment for at least seven days. The maintainer decides and writes the reason in the issue. A
  security objection blocks the change until it is answered. A change to what
  [docs/threat-model.md](docs/threat-model.md) covers updates that file in the same pull request.
- **Disagreement** is settled by the maintainer. Anyone may argue the case again with new facts.
- **This page** changes by pull request, open for seven days.

The maintainer merges their own pull requests after CI, because there is nobody else to review
them. Once there is a second maintainer, every change needs an approving review from someone other
than its author.

## Becoming a maintainer

Maintainers are added by the existing ones, after a person has sent several accepted changes and
reviewed other people's, and has shown they care about the security rules in the threat model. A new
maintainer gets write access first, and admin access once the others agree they can hold it.

## If the maintainer is unavailable

This is a gap, and it is better to say so. One person can create and close issues, merge, tag and
sign releases, and nobody else can do it yet.

What is true now:

- The code is GPL-3.0-or-later and everything needed to build it is in the repository. Anyone can
  fork it and carry on. Nothing is lost if the repository goes quiet.
- Releases are built by CI from a tag, not on anyone's computer. A new maintainer with access to
  the repository's settings can set up the build secrets and release again.
- The Android signing key is the one thing that cannot be replaced. It is backed up offline by the
  maintainer. Without it a phone refuses to update the app in place, and a new key means a new
  install ([docs/releasing.md](docs/releasing.md)).

What will fix it: a second maintainer with admin access to the repository, the `release`
environment and a copy of the signing key's backup, so that either of them can run the project for
a week without the other. Until that person exists, if the maintainer cannot be reached for 30 days
and an issue or discussion asking for them has had no answer, the community may agree on a fork to
carry on, and should say so in a discussion so users can follow.
