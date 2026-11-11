# Contributing to Kite

Thanks for helping. Kite is early, so the best first step is to open or comment on an issue before
writing a large change.

## Workflow

1. Find or open an issue describing the change.
2. Branch from `main` as `type/short-name`, for example `feat/adb-link` or `fix/udp-timeout`.
3. Commit in small steps using conventional subjects (`feat:`, `fix:`, `docs:`, `chore:`, `ci:`).
4. Open a pull request that says `Closes #N` and explains what changed, why, and how you tested it.
5. CI must be green. PRs are squash-merged.

Keep pull requests small and single-purpose.

## Building

You need the stable Rust toolchain (`rust-toolchain.toml` pins the channel and components).

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Guidelines

- Code, comments, commits and user-facing text are in English.
- Memory use and speed matter: no WebView or Electron, bounded buffers, single-threaded tokio unless a
  measurement says otherwise. For changes that affect the running relay, include RSS numbers in the PR.
- New logic and real failure paths get tests.
- Dependencies must have a GPL-3.0-compatible license; `cargo deny check` enforces this.
- Never commit secrets, keystores, `.env` files or local paths.

## Networking check

For anything on the data path: turn on a VPN on the phone, connect Kite, then run
`curl https://ifconfig.me` on the computer. It must print the VPN's IP, and a DNS leak test must list
only the VPN's resolvers.

## License

By contributing you agree that your work is licensed under GPL-3.0-or-later.
