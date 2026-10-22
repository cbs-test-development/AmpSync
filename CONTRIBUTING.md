# Contributing to AmpSync

Thanks for helping. This page covers how to set up, where things live and what a pull request should look like.

## Before you start

- Open an issue first for anything bigger than a small fix, so we can agree on the approach.
- **Never put SharePoint links, tenant or site names, company names, real file names or cookies in an issue, a pull request, a test or a screenshot.** Use `contoso.sharepoint.com` and made-up names.
- Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). It records the design decisions (D1 and onwards) and the interfaces each crate is built against. If your change alters an interface or a decision, update that document in the same pull request.

## Setup

You need Rust (stable, installed by `rustup`; `rust-toolchain.toml` pins the channel and components) and Node.js 24.

Windows: Visual Studio Build Tools with the "Desktop development with C++" workload. WebView2 ships with current Windows.

Ubuntu:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev pkg-config build-essential
```

Then:

```bash
git clone <your fork>
cd AmpSync
cargo build --workspace
cd apps/desktop && npm ci && npm run build   # frontend, needed by the desktop crate
```

Run the desktop app in development with `npm run tauri dev` from `apps/desktop`. Run the CLI with `cargo run -p ampsync-cli -- --help`.

## Repository layout

```
crates/ampsync-core/     all logic: links, auth, SharePoint client, engine, state. No UI.
crates/ampsync-cli/      the `ampsync` command-line program
apps/desktop/            Svelte 5 frontend (Vite)
apps/desktop/src-tauri/  the Tauri 2 desktop crate
design/prototype.html    the approved visual design for the desktop app
docs/                    architecture and user guide
spikes/probe/            throwaway experiments, not part of the build
```

Keep logic in `ampsync-core`. The CLI and the desktop app should stay thin.

## Coding standards

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` must be clean. CI enforces both on Windows and Ubuntu.
- **No hardcoded tenants, sites, URLs or company names** anywhere in code, tests, comments or docs (D5). The tenant host comes from the link the user pastes. The GitHub repository slug is never hardcoded either; it comes from `AMPSYNC_REPO` or `GITHUB_REPOSITORY` at build time.
- Never read, import or write to a user's existing browser profiles (D16). AmpSync owns its own profile directories.
- No telemetry, no network calls other than the user's SharePoint host and the optional GitHub release check.
- Errors that need the user to act (expired sign-in, throttling) are values of `Error`, not panics. An expired session pauses a job, it never fails it (D9).
- Settings that users can change belong in the GUI and `settings.toml`; do not require editing files.
- Write user-facing text in plain language.

## Tests

- Add unit tests next to the code in `ampsync-core`.
- Test transfer behaviour against the **fake SharePoint** used by the integration tests (a local mock server that supports Range requests, 429 with `Retry-After`, session expiry mid-job, eTag changes and corrupted bytes). Do not write tests that need a real tenant or network access.
- Run everything with `cargo test --workspace`.

## Commits and pull requests

- Small, focused commits with a short imperative summary line, for example `Resume partial files after a restart`. Add a body when the reason is not obvious.
- Branch from `main` and open a pull request against it. CI runs on pull requests only; a push to a branch does not build anything.
- Fill in the pull request template.
- Do not add AI tool credits or co-author lines to commits or pull requests (D14). The contributors list should show people.

## How contributors appear in the app

The About page lists the contributors of the repository the app was built from. The list is fetched from the GitHub API during the release build, using the repository named in `AMPSYNC_REPO` (or `GITHUB_REPOSITORY`) and baked into the app, so it also works offline and forks show their own contributors. You appear automatically once a commit authored with your GitHub-linked email address is merged. No file needs editing.

## Releases

Maintainers release by bumping the workspace version in `Cargo.toml` and pushing a tag `vX.Y.Z` that matches it. The release workflow checks that they agree, builds the installers and command-line binaries, and publishes a GitHub Release with `SHA256SUMS.txt`. Nothing is built on pushes to `main`.

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), not in a public issue.
