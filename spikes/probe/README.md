# ampsync-probe

Throwaway diagnostic for AmpSync. It answers one question against **your** SharePoint
Online tenant: *which download approaches work when we only have the browser-session cookies
(`FedAuth` / `rtFa`) and no Entra app registration?*

It signs in (one of four ways), then runs seven independent experiments:

| id | what it checks |
|----|----------------|
| E1 | which cookie names were captured, FedAuth/rtFa present, expiry (values are never recorded) |
| E2 | resolves the URL (sharing link redirects, `/_api/v2.0/shares/u!.../driveItem`) to a folder path |
| E3 | v2.0 drive listing with cookies only; which fields exist (quickXorHash, eTag, downloadUrl, paging...) |
| E4 | classic `_api/web/GetFolderByServerRelativePath` listing |
| E5 | walks up to 2000 items / 60 s: counts, bytes, depth, path lengths, illegal Windows names, 429/503 |
| E6 | Range requests (start and end of the largest file) over four download methods, then a 4x parallel throughput test (up to 64 MiB, nothing saved) |
| E7 | downloads one small file in memory and compares QuickXorHash with the listed hash |

**It never stores cookies or file contents.** Cookie values live only in memory. File data is
read into memory and discarded (E7 keeps a file under 20 MiB in RAM for a few seconds). The
sign-in browser profile is a temporary folder that is deleted afterwards. The report is redacted:
no cookie values, no file/folder/site names (names become `<.ext, size>`, path segments become
`<seg:length>`), no full URLs; only your tenant host name is kept.

## Build

### Windows 11

Needs the Rust MSVC toolchain and the WebView2 runtime (already present on Windows 11).

```powershell
cd spikes\probe
cargo build --release
cargo test
```

Binary: `spikes\probe\target\release\ampsync-probe.exe`

### Ubuntu

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev build-essential pkg-config libssl-dev
cd spikes/probe
cargo build --release
```

The `webview` mode needs a desktop session (X11 or XWayland). The Linux build is untested; the
`edge`/`chrome`/`paste` modes do not need webkit at runtime.

## Run

**Easiest:** double-click `ampsync-probe.exe`. It asks for the folder link and sign-in method, saves `probe-<method>.json` next to the exe, and waits for Enter before closing.

From a terminal, pass the folder URL (quote it). Accepted: a sharing link (`https://x.sharepoint.com/:f:/s/Site/E...`),
the address bar URL of a folder (`...AllItems.aspx?id=%2Fsites%2F...`), or a direct
`https://x.sharepoint.com/sites/Site/Shared Documents/Folder` URL.

Run **all four modes if you can**; each one is an experiment on how to obtain cookies. Use a
different `--out` per mode so the reports do not overwrite each other.

```powershell
# 1. Embedded webview (what the real app would use). Sign in in the window that opens.
.\target\release\ampsync-probe.exe "<folder-url>" --auth webview --out probe-webview.json

# 2. Your installed Edge, fresh temporary profile + remote debugging. Sign in, then press Enter here.
.\target\release\ampsync-probe.exe "<folder-url>" --auth edge --out probe-edge.json

# 3. Your installed Chrome, same idea.
.\target\release\ampsync-probe.exe "<folder-url>" --auth chrome --out probe-chrome.json

# 4. Paste a Cookie header copied from browser DevTools (Network tab > any request to the
#    SharePoint host > Request Headers > Cookie). Input is hidden.
.\target\release\ampsync-probe.exe "<folder-url>" --auth paste --out probe-paste.json
#    If your terminal truncates the long line, save it in a file and use:
#    .\target\release\ampsync-probe.exe "<folder-url>" --auth paste --cookie-file cookie.txt
#    (delete cookie.txt afterwards)
```

On Ubuntu use `./target/release/ampsync-probe` with the same arguments (`--auth edge` looks for
`microsoft-edge`, `--auth chrome` for `google-chrome`/`chromium`).

Extra options: `--no-sso` (webview mode: disable Windows single sign-on with the OS account, to
compare), `--ascii` (PASS/FAIL instead of emoji if your console garbles them).

Notes:
- webview mode continues by itself once SharePoint cookies appear; if it does not, press Enter in
  the terminal when the SharePoint page is visible.
- Sub-sites (`/sites/A/B/Library`) are treated as site `/sites/A` with library `B`; tell us if your
  folder lives in a sub-site.
- The run takes about 1 to 3 minutes. It only does GET requests.

## What to send back

1. The `probe-*.json` file(s) (redacted, safe to share).
2. The terminal summary (the table with the check marks).
3. Anything odd you saw: did SSO sign you in without a prompt, did MFA appear, did the window
   close itself, how many accounts/tenants you are signed in to.

Check before sending: open the JSON and confirm you are comfortable with the tenant host name it
contains. Nothing else identifying should be there.

## Layout

```
src/main.rs         args, orchestration, report + summary
src/urlparse.rs     URL classification, share-id, path encoding
src/auth.rs         captured-cookie type, paste mode
src/webview.rs      wry + tao sign-in window, cookie read, WebView2 SSO option
src/cdp.rs          edge/chrome launch + DevTools Protocol cookie read
src/http.rs         recording HTTP client (cookie jar, manual redirects)
src/experiments.rs  E1..E7
src/qxor.rs         QuickXorHash (rclone test vectors)
src/redact.rs       redaction
src/names.rs        Windows file-name checks
```
