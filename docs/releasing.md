# Releasing Kite

A release is built by `.github/workflows/release.yml` when a tag like `v0.1.0` is pushed, and by
nothing else. It builds the Linux packages and the Android APK from the tagged commit, writes
checksums and SBOMs, attests and signs every file, and leaves a **draft** GitHub release. Nobody
uploads anything by hand, and the release is not public until you press Publish.

## One-time setup

### The Android signing key

The APK must always be signed with the same key, or phones will refuse to update the app. The key
lives in a file that is never committed (`*.jks` is in `.gitignore`) and in four GitHub secrets.
You need a JDK for `keytool`.

1. Make the keystore. Use a long random password and keep it in your password manager.

   ```
   keytool -genkeypair -v -keystore kite-release.jks -alias kite -keyalg RSA -keysize 4096 -validity 10000
   ```

   `keytool` asks for the keystore password, some details about you (they go into the certificate and
   can be left mostly blank), and a key password: press Enter to use the same one.

2. Back up `kite-release.jks` and the password somewhere safe and offline. If you lose either, you
   can never ship an update that installed copies accept.

3. Turn the keystore into one line of text:

   ```
   base64 -w0 kite-release.jks                                                           # Linux
   base64 -i kite-release.jks                                                            # macOS
   [Convert]::ToBase64String([IO.File]::ReadAllBytes("kite-release.jks"))                # Windows PowerShell
   ```

4. Make the protected environment the key lives in. On GitHub: Settings, Environments, New
   environment, named `release`. Then in it:
   - Deployment branches and tags: Selected branches and tags, and add the tag rule `v*`.
   - Required reviewers: yourself. A tag run then waits for your approval before it can use the key.
   - Environment secrets, added by hand (not repository secrets):

     | Secret | Value |
     |--------|-------|
     | `KITE_KEYSTORE_B64` | the line from step 3 |
     | `KITE_KEYSTORE_PASSWORD` | the keystore password |
     | `KITE_KEY_ALIAS` | `kite` (the `-alias` you gave) |
     | `KITE_KEY_PASSWORD` | the key password (the same as the keystore password if you pressed Enter) |

5. Delete the base64 text and keep `kite-release.jks` only in your backup.

### Who may tag

The workflow runs from the files of the tag that started it, so anyone who can push a tag could run
a changed copy of it. The environment above keeps the key out of reach of that; also limit who can
make release tags: Settings, Rules, Rulesets, New tag ruleset, target `v*`, restrict creations,
updates and deletions, and let only the repository admin bypass it.

### What the workflow does with the key

Only the `sign-apk` job sees the four secrets. It runs for tags only, installs nothing and does not
check out the code. It writes the keystore to a temporary file, signs the APK built by an earlier job
with `apksigner`, and deletes the file. It never prints a secret, and it stops with a message that
names the secret when one is missing or the keystore, password or alias does not match.

## Cutting a release

1. In one pull request, set the new version in `Cargo.toml` (`[workspace.package] version`), run
   `cargo update --workspace` so `Cargo.lock` follows, and set `versionName` (the same text) and
   `versionCode` (one more than before) in `apps/android/app/build.gradle.kts`. Merge it.
2. Tag the merge commit on `main` and push the tag:

   ```
   git checkout main && git pull
   git tag -a v0.1.0 -m "Kite 0.1.0"
   git push origin v0.1.0
   ```

   The workflow refuses a tag that does not match both versions, that is not on `main`, or that is not
   `vMAJOR.MINOR.PATCH` with an optional `-suffix`. A suffix (`v0.2.0-rc.1`) makes a pre-release.
3. Approve the `release` environment when GitHub asks, and wait for the Release workflow. If it
   fails, nothing was published: fix the cause, delete the tag (`git push origin :refs/tags/v0.1.0`,
   `git tag -d v0.1.0`) and tag again.
4. Open the draft under Releases, read the notes (they list the `feat`, `fix` and `perf` commits since
   the last tag), install the APK and a package once, and press Publish.

## Trying it first

- **Without a tag:** Actions, Release, Run workflow (it appears once the workflow is on `main`). It
  builds, attests and signs the Sigstore bundles the same way, keeps the files as the `release-files`
  artifact and makes no release. A run without a tag never sees the signing key, so its APK is
  `kite-VERSION-unsigned.apk` and the log says so.
- **A release candidate:** set the version to `0.1.0-rc.1` as in step 1 above and tag `v0.1.0-rc.1`.
  The draft is marked pre-release. Check it as below, then delete it
  (`gh release delete v0.1.0-rc.1 --cleanup-tag`) and do it again for `0.1.0`.

## What a release holds

| File | What it is |
|------|------------|
| `kite_VERSION_amd64.deb` | Debian 12 and Ubuntu 22.04 or newer: command, desktop app, daemon, tray |
| `Kite-VERSION-x86_64.AppImage` | Desktop app and command, without the daemon |
| `kite-VERSION-x86_64-linux.tar.gz` | The `kite` command alone, for any x86-64 Linux with a glibc as new as Ubuntu 22.04's |
| `kite-VERSION.apk` | The phone app, signed with the release key |
| `kite-cli.cdx.json`, `kite-desktop.cdx.json`, `kite-ffi.cdx.json` | CycloneDX SBOMs of the Rust programs |
| `SHA256SUMS` | SHA-256 of every file above |
| `FILE.sigstore.json` | A Sigstore bundle for each of those files |

Each file also has a GitHub build provenance attestation.

## Checking a download

```
gh release download v0.1.0-rc.1 --repo heidari-dev/kite
sha256sum -c SHA256SUMS
gh attestation verify kite_0.1.0-rc.1_amd64.deb --repo heidari-dev/kite
cosign verify-blob --bundle kite_0.1.0-rc.1_amd64.deb.sigstore.json \
  --certificate-identity-regexp '^https://github.com/heidari-dev/kite/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com kite_0.1.0-rc.1_amd64.deb
apksigner verify --print-certs kite-0.1.0-rc.1.apk
```

(Downloading a draft needs a GitHub account with access to the repository; a published release does
not.) The APK's certificate digest must be the same in every release; it is the SHA-256 line
`apksigner` prints, and the workflow log shows it too.
