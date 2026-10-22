# AmpSync

**Download huge SharePoint folders to your disk, reliably. Interrupt it whenever you like; it picks up where it stopped.**

[![Latest release][badge-release]][releases]
[![CI][badge-ci]][ci]
[![License: MIT][badge-license]](LICENSE)
![Platforms: Windows | Ubuntu][badge-platforms]
[![Downloads][badge-downloads]][releases]

![AmpSync downloading a folder](docs/screenshot.png)

<!-- Maintainers: add docs/screenshot.png (a window capture of the Transfers view, with no real site or file names). -->

## Why

Downloading a very large SharePoint or OneDrive folder from the browser means asking the server to build one giant zip. For tens of gigabytes that often times out, stops halfway, or produces an archive that will not open. The usual alternative, the OneDrive sync client, needs an app installed and signed in on a device your organisation manages.

AmpSync takes a different route. You paste the link you were given, sign in the way you normally do, and it copies the files one at a time, in pieces, checking each file before it is kept. If your laptop sleeps, your Wi-Fi drops or your session expires, nothing is lost: run it again and it continues. It only downloads. It never changes anything on SharePoint.

## Download

Get the latest files from the [Releases page][releases]. Pick the one that matches your computer:

| You use | Download | Notes |
|---------|----------|-------|
| Windows 10 / 11 | `AmpSync_<version>_x64-setup.exe` | Recommended. Installs for your user only. No administrator rights needed. |
| Windows, no install | `AmpSync-portable-<version>-x64.exe` | A single file you can run from anywhere, such as a USB drive or a locked-down folder. |
| Ubuntu 22.04 or newer | `AmpSync_<version>_amd64.AppImage` | Make it executable (`chmod +x`), then double-click or run it. |
| Ubuntu / Debian | `AmpSync_<version>_amd64.deb` | Install with `sudo apt install ./AmpSync_<version>_amd64.deb`. |
| Command line, Windows | `ampsync-<version>-x86_64-pc-windows-msvc.zip` | For scripts and servers. See [Command line](#command-line). |
| Command line, Linux | `ampsync-<version>-x86_64-unknown-linux-gnu.tar.gz` | Same, for Linux. |

> [!NOTE]
> macOS is not supported yet.

### Windows shows a blue "Windows protected your PC" screen

The Windows files are not code-signed yet, so SmartScreen does not recognise them. This is expected. Choose **More info**, then **Run anyway**.

If you want to be sure you have the file the project published, compare its checksum with the one in `SHA256SUMS.txt` on the release page:

```powershell
Get-FileHash .\AmpSync_1.0.0_x64-setup.exe -Algorithm SHA256
```

```bash
sha256sum AmpSync_1.0.0_amd64.AppImage
```

## First run

1. **Paste a link.** Copy the link to a SharePoint or OneDrive folder (the one you would normally open in a browser, or a "share" link someone sent you) and paste it into *New download*.
2. **Sign in.** A sign-in window opens inside AmpSync. Use your usual account, including any approval prompt on your phone. The window closes by itself when you are in.
3. **Pick folders.** AmpSync lists what is inside. Tick the folders you want, or leave everything ticked, and choose where to save them.
4. **Download.** Press start. You see speed, time left and which files are in progress. You can pause, close the app and come back later.

On the very first launch a short walkthrough points out each part of the window. You can skip it, and replay it from Settings.

More detail is in the [user guide](docs/user-guide.md).

## Privacy

- **No telemetry.** AmpSync does not collect usage data and has no account of its own.
- **Your sign-in stays on your machine**, in the operating system's keychain (Windows Credential Manager, or the Secret Service on Ubuntu).
- **It never touches your browser profiles.** It does not read, copy or sign in to Edge, Chrome, Teams or anything else you use. Its own sign-in window keeps a separate profile in AmpSync's own folder, and "Sign out" deletes it.
- **It talks to two places only:** your SharePoint tenant (to list and download the files you asked for) and GitHub (an optional once-a-day check for a newer release, which you can turn off in Settings). It never installs updates by itself.

## Command line

The `ampsync` program does the same job without a window. Replace the example address with your own link.

```bash
# Sign in once; the session is stored in your OS keychain
ampsync login "https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN"

# Look at what is in a folder
ampsync ls "https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN" --depth 2

# Download it (run the same command again to resume or refresh)
ampsync get "https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN" ./projects

# Only some files, and be gentle on the network
ampsync get "<link>" ./projects --include "*.pdf" --exclude "archive/**" --parallel 2 --limit 20M
```

| Command | What it does |
|---------|--------------|
| `ampsync login <url> [--method webview\|edge\|chrome\|paste]` | Sign in and store the session |
| `ampsync logout <host>` | Forget the session for a host, e.g. `contoso.sharepoint.com` |
| `ampsync sessions` | List stored sessions |
| `ampsync ls <url> [--depth N]` | List a remote folder |
| `ampsync get <url> <dest> [options]` | Download. Options: `--include`, `--exclude`, `--parallel`, `--limit`, `--redownload`, `--prune`, `--yes`, `--json` |

Exit codes: `0` success, `1` error, `2` bad usage, `3` sign-in expired (sign in again and rerun to resume), `4` some files failed.

> [!TIP]
> `--prune` deletes local files that no longer exist on SharePoint. Without it, AmpSync never deletes anything.

## FAQ

**Does it need administrator rights?** No. The installer works for the current user only, and the portable file needs no install at all.

**Does it work with multi-factor authentication?** Yes. You sign in exactly as you would in a browser, including approval prompts.

**Can I stop and resume?** Yes. Partly downloaded files continue from where they stopped, files that are already complete and unchanged are skipped, and a sign-in that expires pauses the download instead of failing it.

**How do I know a download is not corrupt?** Each file is written under a temporary name, its size is checked and SharePoint is asked again to confirm the file did not change while it downloaded, and only then is it renamed into place.

**Does it work on a Mac?** Not yet.

**Will it change or delete anything on SharePoint?** No. It is download-only.

<details>
<summary><strong>My company blocks the sign-in window</strong></summary>

Some organisations stop apps from signing in with an embedded window. AmpSync then offers to use your installed Edge or Chrome with a temporary, separate profile that is deleted afterwards, and as a last resort lets you paste a session. See the [user guide](docs/user-guide.md#signing-in).

</details>

## Building from source

You need Rust (stable), Node.js 24 and the platform tools below.

<details>
<summary><strong>Windows</strong></summary>

- [Rust](https://rustup.rs) with the MSVC toolchain, and the Visual Studio Build Tools with the "Desktop development with C++" workload
- Node.js 24
- Microsoft Edge WebView2 Runtime (already present on current Windows 10 and 11)

</details>

<details>
<summary><strong>Ubuntu 22.04 / 24.04</strong></summary>

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev pkg-config build-essential
```

</details>

```bash
# Command line
cargo build --release -p ampsync-cli

# Desktop app
cd apps/desktop
npm ci
npm run tauri build
```

Run the checks with `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. More in [CONTRIBUTING.md](CONTRIBUTING.md).

## Contributing

Bug reports, ideas and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first, and never include SharePoint links or file names in an issue. Security problems go through the process in [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)

<!-- Badge and link targets. The repository slug appears only in this block. -->

[releases]: https://github.com/cbs-test-development/AmpSync/releases/latest
[ci]: https://github.com/cbs-test-development/AmpSync/actions/workflows/ci.yml
[badge-release]: https://img.shields.io/github/v/release/cbs-test-development/AmpSync?sort=semver
[badge-ci]: https://img.shields.io/github/actions/workflow/status/cbs-test-development/AmpSync/ci.yml?label=CI
[badge-downloads]: https://img.shields.io/github/downloads/cbs-test-development/AmpSync/total
[badge-license]: https://img.shields.io/badge/license-MIT-blue
[badge-platforms]: https://img.shields.io/badge/platforms-Windows%20%7C%20Ubuntu-lightgrey
