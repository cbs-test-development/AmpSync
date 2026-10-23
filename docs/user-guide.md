# AmpSync user guide

This guide walks through a download from start to finish and explains what AmpSync does at each step. For a short version, see the [README](../README.md).

## Contents

- [What AmpSync does](#what-ampsync-does)
- [Your first download](#your-first-download)
- [Checking the disk first](#checking-the-disk-first)
- [Signing in](#signing-in)
- [When your sign-in expires](#when-your-sign-in-expires)
- [When the disk is full](#when-the-disk-is-full)
- [Parallel lanes: Auto](#parallel-lanes-auto)
- [Big files and the fill map](#big-files-and-the-fill-map)
- [Progress on the taskbar and in the tray](#progress-on-the-taskbar-and-in-the-tray)
- [Notifications](#notifications)
- [Startup animation](#startup-animation)
- [Settings explained](#settings-explained)
- [Running from the command line](#running-from-the-command-line)
- [Where your files and data live](#where-your-files-and-data-live)
- [Running a download again](#running-a-download-again)
- [Removing everything](#removing-everything)
- [Troubleshooting](#troubleshooting)

## What AmpSync does

AmpSync copies a SharePoint or OneDrive folder to your disk. It is download-only: it never uploads, edits or deletes anything on SharePoint. Large files are fetched in pieces, every file is checked before it is kept, and an interrupted download continues where it stopped.

## Your first download

1. Open AmpSync. The first launch shows a short walkthrough; skip it or follow it, and replay it any time from Settings.
2. Go to **New download** and paste the link. It can be a folder address from your browser or a "share" link sent to you. The SharePoint address is taken from the link, so there is nothing to configure.
3. Sign in if asked (see [Signing in](#signing-in)).
4. AmpSync lists the folder tree. Large folders appear as they are discovered. Tick what you want; everything is ticked by default.
5. Choose **Save to**, and decide what happens to files that already exist: *Skip if unchanged* (default) or *Download again*.
6. Start. The **Transfers** view shows progress, speed, time left, the files in flight and the queue. Add more links and they run one after another.

Pausing, closing the window and restarting the computer are all safe.

## Checking the disk first

Under the folder tree, a line next to the selected total shows what the selection needs and what the destination has free, for example **Needs 31.2 GB · 112 GB free on D:**. The figure on the left is the size of what is ticked. It does not know which files are already on your disk (AmpSync only finds that out when the download starts), so with *Skip if unchanged* the real need can be lower.

AmpSync wants a gigabyte of room to spare on top of that for its own temporary files. If the selection does not fit, the line turns red, **Add to queue** is disabled, and the message under it says how much to free up:

> Not enough space on D:. Free up 6.3 GB or choose another folder.

It looks again whenever you change the **Save to** folder, when you come back to the window, and just before the download is added, so freeing space or picking another drive takes effect straight away. On Ubuntu the drive is named by its mount point (`/home`, for example). Nothing here uses the network; it only asks your computer how much space is free.

## Signing in

AmpSync uses your normal SharePoint sign-in, including multi-factor authentication. It tries three methods in order, and moves on only if the previous one does not work.

### 1. AmpSync window

A sign-in window opens inside the app and shows the usual Microsoft page. When you are signed in, the window closes by itself.

AmpSync keeps a **separate browser profile of its own** for this window, so you do not have to approve a prompt every time. It is not connected to Edge, Chrome or anything else on your computer.

- On Windows, if your computer is signed in with a work or school account, AmpSync can use it for single sign-on, so a managed machine may sign in with no prompt at all. You can switch this off in Settings.
- Saving passwords in that profile is off by default and is a switch in Settings.
- When AmpSync starts, it quietly checks the sites you have used. If one needs a password or approval again, the sign-in window opens right then, before any download begins, rather than interrupting you halfway through.

### 2. Edge or Chrome

Some organisations do not allow sign-in inside apps. If the AmpSync window is blocked, AmpSync can open your installed Microsoft Edge or Google Chrome with a **temporary profile created just for this**. Sign in there as usual. AmpSync captures only the SharePoint session, then closes the browser and deletes the temporary profile. Your own browser profile, bookmarks, saved passwords and other accounts are never read.

### 3. Paste a session

The last resort, for example on a computer without a screen. Sign in with a browser on another machine, copy the `Cookie` header of a SharePoint request from the browser's developer tools, and paste it into AmpSync (or give it to `ampsync login --method paste`).

> [!WARNING]
> A pasted cookie is as powerful as your password for that site. Never share it, never paste it into an issue or chat, and sign out of SharePoint in the browser when you are done.

### Where the session is stored

Sessions are stored per SharePoint address in your operating system's keychain: Windows Credential Manager, or the Secret Service (GNOME Keyring or KWallet) on Ubuntu. The **Sign-ins** view lists them and lets you remove each one.

## When your sign-in expires

SharePoint sessions end after a while, and very large downloads can outlast one. When AmpSync notices:

1. The download **pauses**. It does not fail, and nothing already downloaded is lost.
2. AmpSync first tries to renew the session silently.
3. If Microsoft needs you to type a password or approve a prompt, the sign-in window opens. Sign in and the download resumes by itself.

If you are away, it stays paused until you return. With the command line, the exit code is `3`; sign in again with `ampsync login` and run the same `ampsync get` command to continue.

## When the disk is full

Free space can run out during a long download, because other programs write to the same drive. When a write fails because the disk is full:

1. The download **pauses**. It does not fail, and nothing already saved is touched or deleted.
2. The Transfers view shows a banner such as **D: is full: 6.3 GB more needed**, the download is labelled *Paused: disk full*, and its queue entry says *needs space*. This is different from a pause you asked for and from a sign-in pause.
3. Free up space (or move the destination's contents elsewhere), then press **Retry**. The banner shows how much is free right now, and says when there is room again. Retry carries on from where the download stopped. If the disk is still full, the download pauses again with a fresh message.

If you quit AmpSync while it is waiting for space, the download goes back in the queue and starts again the next time you open the app. If the disk is still full, it pauses again with the same banner.

## Parallel lanes: Auto

A lane is one transfer running at the same time as the others. By default AmpSync is on **Auto**: it starts with 4 lanes and, every 10 seconds of active transfer, tries one more. If that did not make the download at least 5% faster it takes the lane back and waits a minute before trying again. When SharePoint asks it to slow down it halves the lanes and waits before adding any. It stays under about 80% of SharePoint's request limit for one user, using larger pieces for new files when it gets close. It never uses fewer than 2 lanes or more than 16.

The **Parallel lanes** panel in Transfers is headed **Auto · 7 lanes** (or just **7 lanes** if you set a fixed number). Every lane is listed, in two columns once there are more than 8, so you can see what each is doing. A lane that is being taken away finishes its file first and is marked *closing*.

On the speed chart, a dotted line marks each moment the number of lanes changed, with the new count next to it. Hover over it (or look at the list below the chart if you use a screen reader) to read why in plain words, for example *Speed was still rising, so AmpSync tried one more*, or *SharePoint asked AmpSync to slow down, so it halved the lanes*.

Pick a number from 1 to 16 in **Settings** if you want a fixed count instead (for example, to leave bandwidth for a video call). Choose **Auto** again to hand control back. Either way the change applies to the next download that starts.

## Big files and the fill map

Files over 64 MB are fetched in pieces (ranges), several at a time. For those, the lane shows a **fill map** under the file name: a strip with one cell for each piece, squeezed into at most 120 cells for very big files.

- Dark: not fetched yet.
- Dim amber: being fetched right now.
- Solid amber: saved. A cell that stands for several pieces fills from the bottom as they arrive.
- Outlined: a piece that failed and is being fetched again.

Hover over a cell to read it, for example *range 412 of 5,021 · 16 MiB*. For a cell that stands for several pieces it gives the span and how many are saved. When a download resumes, the map starts filled where it left off.

To look closer, click a file in the **Files** list (files with a map have a small striped mark). The **Selected file** panel replaces the queue and shows a larger map, a count of saved pieces and a key. Click **Close** to get the queue back. Only the most recent 500 files keep a map. If your system is set to reduce motion, pieces simply appear as saved, without the brief flash.

## Progress on the taskbar and in the tray

While a download runs you do not need the AmpSync window in front.

- **Taskbar (Windows).** AmpSync's taskbar button fills like a progress bar. It is green while downloading, yellow when the download is paused, waiting for you to sign in, or stopped because the disk is full, and red if it stopped with an error. The red bar goes away the next time you open the window. On Ubuntu the same bar appears where the desktop supports it (Unity launcher API); other desktops ignore it.
- **Tray icon.** A ring around the AmpSync icon fills clockwise as the download progresses (grey while paused). Hover over it for the numbers, for example `AmpSync · 42% · 9 min left`. The time left is left out while the download is paused or the speed is too low to give an honest estimate. The tray icon returns to normal when nothing is downloading.

## Notifications

AmpSync can tell you when something needs your attention, so you can leave a big download running in the background. It is **opt-in**: the first time you start a download, a small question appears at the top of the Transfers view, "Get a notification when downloads finish?" Choose **Yes** or **No thanks**; AmpSync never asks again. You can change your mind any time under **Settings, Notify when downloads finish**.

When it is on, you get a notification for exactly three things, and only while AmpSync's window is in the background:

- a download **finished** ("Your folder is ready, 3,906 files"),
- a download **stopped with an error**,
- a **sign-in is needed** to carry on.

Pausing, stopping a download yourself and other routine events never notify. Clicking a notification brings AmpSync to the front. Nothing is sent anywhere: notifications are drawn by your operating system on your own computer.

## Startup animation

When AmpSync opens it shows a short animation while it loads and checks your saved sign-ins. The line under the name tells you what it is really doing (checking sign-ins, how many downloads are waiting). It stays for at least three seconds and leaves as soon as AmpSync is ready, or after six seconds at most. Click or press any key to skip it.

Choose how it plays under **Settings, Startup animation**:

- **On** (default): the full animation.
- **Reduced motion**: a still logo and name with the status line, then a quick fade. Also used automatically when your operating system's "reduce motion" setting is on.
- **Off**: no animation; the window opens as soon as the app is ready.

The line at the bottom ("built by @name and N contributors") comes from the contributor list of the build you are running, and is left out when a build has none.

## Settings explained

Settings save automatically.

| Setting | What it does |
|---------|--------------|
| Default folder | Pre-fills **Save to** in New download. |
| Files at the same time | **Auto** (default) lets AmpSync choose the number of lanes, or fix it at 1 to 16. See [Parallel lanes: Auto](#parallel-lanes-auto). More is not always faster; if SharePoint slows you down, try fewer. |
| Speed limit | Caps total download speed, so a big job does not use the whole connection. Empty means unlimited. |
| When the window closes | **Ask**, **Keep going in tray** (transfers continue in the system tray), or **Pause** (pauses and quits). |
| Check for new versions | Looks at GitHub Releases once a day. Nothing else is sent, and AmpSync never installs an update by itself. |
| Notify when downloads finish | Opt-in notifications for finished, failed and sign-in-needed, only while the window is in the background. See [Notifications](#notifications). |
| Startup animation | **On**, **Reduced motion** or **Off**. See [Startup animation](#startup-animation). |
| Remember passwords in the sign-in window | Off by default. |
| Use the Windows work account (Windows) | Single sign-on with the account Windows is signed in with. On by default. |
| Replay the walkthrough | Shows the first-run tour again. |
| Remove all AmpSync data | Signs out everywhere and deletes AmpSync's settings, sign-ins and history, then restarts it fresh. Your downloaded files stay. See [Removing everything](#removing-everything). |

## Running from the command line

`ampsync get <link> <folder>` does what the window does. This section covers what is specific to it; the options are listed in the [README](../README.md#command-line).

### Disk space

Before anything downloads, `get` works out how much is still to download (files that are already up to date and parts already on disk do not count) and compares it with the free space on the destination's disk. The summary shows both:

```
Disk space:   needs 31.2 GiB, 112.0 GiB free on D:
```

AmpSync keeps 1 GiB free on top of what the download needs. If the disk is too small, nothing starts, the exit code is `1`, and the message says how much to free up (the 1 GiB is included, so freeing exactly that much is enough):

```
error: Not enough space on D:. Needs 31.2 GiB, 24.9 GiB free. Free up 7.3 GiB or choose another folder.
```

The disk is named by its drive letter on Windows (`D:`) and by its mount point on Ubuntu (`/home`). Add `--ignore-space` to start anyway, for example when you know space will be freed by the time it is needed. If the operating system cannot report the free space, AmpSync warns and carries on.

If the disk fills up **while downloading** (other programs use it too), the download pauses and nothing is deleted: finished files and partial files stay. A line like this appears:

```
Disk full on D: (12.0 MiB free, 20.0 GiB still to download). Paused. Free up space and it continues by itself (checking every 15s, or press Enter to try now); Ctrl+C stops and keeps what is downloaded.
```

Every 15 seconds AmpSync looks at the free space and carries on when trying again is worthwhile: when everything that is left plus the 1 GiB spare fits, or when at least 1 GiB more is free than when the disk filled up (enough for real progress; if it fills again it simply pauses again). Free space that has not grown at all never triggers a retry on its own, because some disks (quotas, network shares) report room but still refuse writes; press Enter on a terminal to force a retry. Ctrl+C stops cleanly and keeps the part files, and running the same command later resumes.

### Simultaneous transfers

Unless you say otherwise, AmpSync chooses how many files and pieces download at once. It starts with 4, tries one more when the speed is still rising, takes it back when it does not help, and halves the number when SharePoint asks it to slow down (between 2 and 16 in total). Each change is shown once, in plain words:

```
auto: 5 lanes (throughput still rising)
auto: 4 lanes (the last lane did not help, holding here)
auto: 2 lanes (SharePoint asked us to slow down)
auto: 4 lanes (close to SharePoint's request limit, so no more lanes and bigger pieces)
```

`--parallel N` (1 to 16) switches this off and uses exactly N. There is no `--auto` flag: leaving `--parallel` out is automatic. The summary before the download says which mode is used (`Transfers:    automatic (2 to 16, starting at 4)` or `Transfers:    6 at a time (fixed)`).

### `--json` for scripts

`--json` prints one JSON object per line on standard output instead of the progress display, and never asks for confirmation. Messages and errors still go to standard error, and the exit code is the same as without `--json`. Every object has a `type`. Sizes are bytes, durations are milliseconds (`*_ms`), paths are relative to the destination with `/` separators.

| `type` | Fields | When |
|--------|--------|------|
| `planned` | `files`, `bytes`, `skipped`, `skipped_bytes` | The job starts. Skipped files are only counted here, never listed |
| `file_started` | `rel_path`, `size`, `lane`, `done_ranges` | A file's first request goes out. `done_ranges` lists the ranges (by index) already on disk when a download resumes; empty otherwise |
| `range_done` | `rel_path`, `index`, `count` | One piece of a large file is on disk (`index` of `count`, counted from 0). Sent once per piece, never for small files; many per large file, and never shown without `--json` |
| `lanes_changed` | `lanes`, `reason` | The automatic tuning changed the number of simultaneous transfers. `reason` is `probe_up`, `no_gain`, `throttled` or `budget` (with `budget` the count may be unchanged). Not sent for the initial 4, nor with `--parallel N` |
| `progress` | `bytes_done`, `bytes_total`, `rate_bps` | Overall progress, about four times a second |
| `lane_progress` | `lane`, `rel_path`, `offset`, `size` | Progress of one transfer |
| `file_verified` | `rel_path`, `by` | A file is complete and checked (`by`: `hash`, `size_and_etag`, `size_and_mtime`) |
| `file_skipped` | `rel_path` | A file was skipped while the job ran |
| `file_failed` | `rel_path`, `error`, `will_retry` | A file failed; `will_retry` says whether another attempt follows |
| `throttled` | `retry_after_ms` | SharePoint asked to slow down; all transfers wait |
| `auth_expired` | `host` | The sign-in expired; the job is paused (a script cannot sign in again, so the command ends with exit code `3`) |
| `disk_full` | `path`, `needed`, `free` | A write failed because the disk is full. `path` is the file being written, `needed` the bytes still to download, `free` the space on the destination's disk. The job is now paused and nothing was deleted |
| `paused`, `resumed` | | The job paused or continued |
| `finished` | `downloaded`, `skipped`, `failed`, `bytes`, `elapsed_ms`, `failures` | The job ran to the end |
| `failed` | `error` | The job could not run |
| `cancelled` | | The job was stopped |

Examples:

```json
{"type":"lanes_changed","lanes":7,"reason":"probe_up"}
{"type":"disk_full","path":"D:/out/big.bin","needed":33500000000,"free":12582912}
{"type":"range_done","rel_path":"a/big.bin","index":3,"count":40}
{"type":"file_started","rel_path":"a/big.bin","size":671088640,"lane":1,"done_ranges":[0,1,5]}
```

With `--json` and a full disk, nothing is printed besides the `disk_full` and `paused` events. The command waits and checks the free space every 15 seconds, using the same rule as above, and then resumes (you see a `resumed` event). There is no Enter-to-retry in this mode. To give up, stop the process: the next run resumes where it stopped.

The disk-space check before the start applies to `--json` too: with too little space the command prints the message to standard error, prints no events, and exits with `1`.
## Where your files and data live

**Your downloaded files** are in the folder you chose, and only the files. Once a download finishes, AmpSync leaves nothing behind in that folder, so you can share it, zip it or put it under version control as it is. While a file is still downloading it sits next to its final name with the suffix `.ampsync-part`; it is renamed to its real name only after it has been verified, and any leftover `.ampsync-part` file is removed when the download completes. (If a file fails part-way, its partial copy is kept so the next run can continue it.)

**The job's progress** (which pieces of which files are done) is kept in AmpSync's own app data folder, one small folder per destination (`jobs\<id>` inside the app data folder below, with a `dest.txt` that says which destination it belongs to). That is what lets a rerun resume. If you copy or move a finished download somewhere else and run the same link against the new location, AmpSync recognises files whose size and modified time match SharePoint and skips them, without downloading anything again. Old versions kept a hidden `.ampsync` folder inside the destination; AmpSync moves that into the app data folder the next time it runs there.

> [!NOTE]
> Windows does not allow some characters in file names (`< > : " | ? *`, and names ending in a dot or space). AmpSync replaces them with a visible lookalike and remembers the original name in its job state.

**AmpSync's own data** is in per-user folders:

| What | Windows | Ubuntu |
|------|---------|--------|
| Settings (`settings.toml`) | `%APPDATA%\AmpSync\config` | `~/.config/ampsync` |
| App data: each job's progress (`jobs`), queue, sign-in window profile (`signin-profile`) | `%LOCALAPPDATA%\AmpSync\data` | `~/.local/share/ampsync` |
| The app window's own WebView cache | `%LOCALAPPDATA%\io.github.ampsync.desktop` | `~/.local/share/io.github.ampsync.desktop` |
| Sign-in sessions | Windows Credential Manager | Secret Service (keyring) |

## Running a download again

Run the same link and destination again at any time. AmpSync lists the folder, skips files that are unchanged, resumes partly downloaded files, and downloads new or changed files. Local files that no longer exist on SharePoint are left alone unless you use `--prune` on the command line.

## Removing everything

AmpSync keeps its own files in a few per-user folders and your sign-ins in the system keychain; your download folders hold nothing of AmpSync. Removing everything deletes every saved sign-in, the settings, the job progress and download history, the queue, the sign-in window's profile and the app window's cache. **Downloaded files are never touched.** Pick whichever fits how you installed it:

- **Windows installer (uninstalling).** *Settings, Apps, AmpSync, Uninstall.* The uninstaller shows the box **Also remove my sign-ins, settings and download history (your downloaded files are not touched)**. It is **ticked by default**. Leave it ticked to remove everything, including the entries in Windows Credential Manager, or untick it to keep your sign-ins and settings for a later reinstall. If AmpSync is running, the uninstaller asks to close it first. Silent and update installs never remove your data. For scripted removal, run `uninstall.exe /S /REMOVEALL`.
- **Without uninstalling (any install).** Open **Settings** and choose **Remove all data** under *Remove all AmpSync data*, then confirm. AmpSync signs you out everywhere, deletes its data, closes and starts again as if it was just installed (the walkthrough shows again).
- **Portable `.exe`, AppImage, command line, or Ubuntu .deb.** Run:

  ```
  ampsync purge        # asks first; add --yes to skip the question
  ```

  Close the AmpSync window before running it.

**Ubuntu .deb:** `sudo apt remove ampsync` cannot reach per-user data (it runs as root and cannot see each user's keyring and home folder), so it leaves your sign-ins and settings behind. Run `ampsync purge` or the Settings action **before** `apt remove`. The portable `.exe` is just a file; delete it after purging.

If you prefer to do it by hand:

1. In **Sign-ins**, choose **Sign out** for every site. This deletes the stored session and the sign-in window's profile.
2. Uninstall AmpSync (Windows: *Settings, Apps*; Ubuntu: `sudo apt remove ampsync` for the .deb, or delete the AppImage).
3. Delete AmpSync's data folders if they still exist:

   **Windows** (PowerShell):

   ```powershell
   Remove-Item -Recurse -Force "$env:APPDATA\AmpSync", "$env:LOCALAPPDATA\AmpSync", "$env:LOCALAPPDATA\io.github.ampsync.desktop" -ErrorAction SilentlyContinue
   ```

   **Ubuntu**:

   ```bash
   rm -rf ~/.config/ampsync ~/.local/share/ampsync ~/.local/share/io.github.ampsync.desktop
   ```

4. Optionally delete the downloaded files. (A destination downloaded with an older version may still have a hidden `.ampsync` folder; it is safe to delete.)

If you used the command line, also run `ampsync sessions` and `ampsync logout <host>` for anything listed.

## Troubleshooting

- **"Sign-in expired" keeps appearing.** Sign in again from the Sign-ins view. If your organisation shortens session lifetimes, a long job will pause more than once; it resumes each time.
- **The sign-in window is blank or blocked.** Choose the Edge or Chrome method instead.
- **Downloads are slow or pause for a while.** SharePoint asks apps to slow down when it is busy, and AmpSync obeys. On Auto it backs off by itself; with a fixed number, try fewer files at the same time.
- **"D: is full" and the download stopped.** See [When the disk is full](#when-the-disk-is-full). Free some space and press Retry.
- **A file failed.** AmpSync retries automatically. If it still fails, run the download again; only the missing pieces are fetched.
- **Reporting a problem.** Open About, choose **Copy diagnostics**, and paste it into a [new issue](https://github.com/cbs-test-development/AmpSync/issues/new/choose). Do not include SharePoint links, file names or cookies.
