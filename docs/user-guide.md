# AmpSync user guide

This guide walks through a download from start to finish and explains what AmpSync does at each step. For a short version, see the [README](../README.md).

## Contents

- [What AmpSync does](#what-ampsync-does)
- [Your first download](#your-first-download)
- [Signing in](#signing-in)
- [When your sign-in expires](#when-your-sign-in-expires)
- [Settings explained](#settings-explained)
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

## Settings explained

Settings save automatically.

| Setting | What it does |
|---------|--------------|
| Default folder | Pre-fills **Save to** in New download. |
| Files at the same time | How many transfers run in parallel (1 to 16, default 4). More is not always faster; if SharePoint slows you down, try fewer. |
| Speed limit | Caps total download speed, so a big job does not use the whole connection. Empty means unlimited. |
| When the window closes | **Ask**, **Keep going in tray** (transfers continue in the system tray), or **Pause** (pauses and quits). |
| Check for new versions | Looks at GitHub Releases once a day. Nothing else is sent, and AmpSync never installs an update by itself. |
| Notify when a download finishes | Uses your system's notifications. |
| Remember passwords in the sign-in window | Off by default. |
| Use the Windows work account (Windows) | Single sign-on with the account Windows is signed in with. On by default. |
| Replay the walkthrough | Shows the first-run tour again. |
| Remove all AmpSync data | Signs out everywhere and deletes AmpSync's settings, sign-ins and history, then restarts it fresh. Your downloaded files stay. See [Removing everything](#removing-everything). |

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
- **Downloads are slow or pause for a while.** SharePoint asks apps to slow down when it is busy, and AmpSync obeys. Try fewer files at the same time.
- **A file failed.** AmpSync retries automatically. If it still fails, run the download again; only the missing pieces are fetched.
- **Reporting a problem.** Open About, choose **Copy diagnostics**, and paste it into a [new issue](https://github.com/cbs-test-development/AmpSync/issues/new/choose). Do not include SharePoint links, file names or cookies.
