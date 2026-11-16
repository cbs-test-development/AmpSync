# Running ampsync in Docker or other headless Linux containers

This guide is for building an automated pipeline that downloads SharePoint data with the Linux command-line program, `ampsync`, inside a container. It works the same with Docker and Podman; the examples use `docker`, and you can replace it with `podman`.

> [!NOTE]
> The features described here (`--cookie-file -` and `AMPSYNC_SESSION_STORE`) are new in the next release. Earlier versions do not have them.

## What is different in a container

The Linux command-line release is a headless build: it has no embedded sign-in window. A container also has no browser to open and no screen, so the usual ways of signing in do not apply. Two things change:

- **You sign in with a pasted cookie.** You sign in to SharePoint in a browser on a normal computer, copy the `Cookie` header of a SharePoint request, and give it to `ampsync login` from a file or from standard input. No terminal is needed.
- **The session is kept in a file, not in the keychain.** On Linux, AmpSync normally stores sessions in the Secret Service over D-Bus, which a container does not have. Set `AMPSYNC_SESSION_STORE=file` and AmpSync writes them under its data folder instead.

## Get the cookie

Do this once, on a computer with a browser:

1. Sign in to your SharePoint site as you normally do.
2. Open the browser's developer tools and go to the **Network** tab. Reload the page.
3. Click any request to the SharePoint site and find the **`Cookie`** request header.
4. Copy its whole value and save it to a file, for example `cookie.txt`.

See [Paste a session](user-guide.md#3-paste-a-session) in the user guide for the same steps in context.

> [!WARNING]
> A cookie is as powerful as your password for that site. Treat `cookie.txt` and the stored session file as passwords: do not bake them into an image, do not commit them, and do not paste them into chat or an issue. Delete `cookie.txt` once the login has worked.

## Sign in without a terminal

Use `--cookie-file` (it implies `--method paste`):

```bash
ampsync login 'https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1' --cookie-file cookie.txt
```

To read the cookie from standard input instead, pass `-` as the file name. This is the way to use a container secret, so the cookie never has to sit in a file you manage:

```bash
ampsync login "$URL" --cookie-file - < /run/secrets/sp_cookie
```

### Quote the URL

SharePoint links often contain `&`. Unquoted, the shell treats `&` as "run in the background", cuts the link there, and the command seems to stop at the prompt:

```bash
# Wrong: the shell runs everything before the & in the background
ampsync login https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1 --cookie-file cookie.txt
[1]+  Stopped

# Right: single quotes (or double quotes, if you use a variable)
ampsync login 'https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1' --cookie-file cookie.txt
ampsync login "$URL" --cookie-file cookie.txt
```

Always quote the link in scripts, in `Dockerfile` `CMD` lines and in `docker run` arguments.

## Where the session is stored

With `AMPSYNC_SESSION_STORE=file`, each session is a file at `<data dir>/sessions/<host>.json`. The file has permissions `0600` and the folder `0700`.

| Variable | Values | Default | Effect |
|----------|--------|---------|--------|
| `AMPSYNC_SESSION_STORE` | `file` or `keychain` | `keychain` | Where sessions are kept. Without the variable nothing changes. |
| `AMPSYNC_DATA_DIR` | a folder | `~/.local/share/ampsync` | AmpSync's data folder: sessions, settings and the progress of each job. |

Mount a volume on `AMPSYNC_DATA_DIR` so the session, and the progress of unfinished downloads, survive a container restart. Without it, you sign in again every time the container is recreated.

## An example image

The release archive is `ampsync-<version>-x86_64-unknown-linux-gnu.tar.gz` on the [Releases page](https://github.com/cbs-test-development/AmpSync/releases). It holds one folder of the same name, containing `ampsync`, `LICENSE` and `README.md`. Release tags are `v<version>`.

```dockerfile
FROM debian:bookworm-slim

# Replace with the version you want, e.g. 1.3.0
ARG AMPSYNC_VERSION

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    name="ampsync-${AMPSYNC_VERSION}-x86_64-unknown-linux-gnu"; \
    curl -fsSL -o /tmp/ampsync.tar.gz \
      "https://github.com/cbs-test-development/AmpSync/releases/download/v${AMPSYNC_VERSION}/${name}.tar.gz"; \
    tar -xzf /tmp/ampsync.tar.gz -C /tmp; \
    install -m 0755 "/tmp/${name}/ampsync" /usr/local/bin/ampsync; \
    rm -rf /tmp/ampsync.tar.gz "/tmp/${name}"

ENV AMPSYNC_SESSION_STORE=file \
    AMPSYNC_DATA_DIR=/data/ampsync

VOLUME /data/ampsync
WORKDIR /work
ENTRYPOINT ["ampsync"]
```

Build it:

```bash
docker build --build-arg AMPSYNC_VERSION=1.3.0 -t ampsync .
```

To check the download against the published checksums, compare it with `SHA256SUMS.txt` on the release page.

> [!NOTE]
> The Linux binary needs only glibc; TLS is built in, so no OpenSSL package is required. It runs on Debian 12, Ubuntu 22.04 or newer, and Rocky Linux / RHEL 9. Any of these works as the base image; the Debian one above is just the smallest.

## Running it

Create a named volume once, sign in, then download. The cookie is passed on standard input, so it is not written into the container:

```bash
docker volume create ampsync-data

# Sign in (reads the cookie from stdin)
docker run --rm -i -v ampsync-data:/data/ampsync ampsync \
  login 'https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1' --cookie-file - < cookie.txt
rm cookie.txt

# Download into ./projects on the host
docker run --rm -v ampsync-data:/data/ampsync -v "$PWD/projects:/work/projects" ampsync \
  get 'https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1' /work/projects
```

With Podman, the same commands work with `podman` in place of `docker`. On a host with SELinux, add `:Z` to the bind mount (`-v "$PWD/projects:/work/projects:Z"`). With rootless Podman, a bind-mounted folder may need `--userns=keep-id` to be writable.

You can also keep the cookie as an orchestrator secret and mount it at `/run/secrets/sp_cookie`:

```bash
ampsync login "$URL" --cookie-file - < /run/secrets/sp_cookie
```

## A pipeline script

`get` never asks questions when it has no terminal, so it is safe in a pipeline. Rerunning the same command resumes or refreshes, so a script can simply run it again. This example runs inside the container (or any machine with `ampsync` installed) and reacts to the exit code:

```bash
#!/usr/bin/env bash
set -u

URL='https://contoso.sharepoint.com/:f:/s/Projects/EaBcDeFgHiJkLmN?e=abc&xyz=1'
DEST=/work/projects

ampsync get "$URL" "$DEST" --include "*.pdf" --parallel 4 --json > events.jsonl
code=$?

case $code in
  0) echo "done" ;;
  3) echo "session expired: sign in again with a fresh cookie, then rerun" >&2 ;;
  4) echo "finished, but some files failed: rerun to retry them" >&2 ;;
  *) echo "ampsync failed with exit code $code" >&2 ;;
esac
exit $code
```

`--json` prints one JSON object per line on standard output (progress events, and a final `finished` or `failed`); messages and errors still go to standard error. The event list is in the [user guide](user-guide.md#--json-for-scripts). All the options of `get` and `ls` are in the [README](../README.md#command-line). To look at a folder first:

```bash
ampsync ls "$URL" --depth 2
```

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Error (including not enough disk space, and Ctrl+C) |
| `2` | Bad usage, or a link that is not a SharePoint folder link |
| `3` | The sign-in expired. Sign in again and rerun the same command to resume |
| `4` | The job finished but some files failed. Rerun to retry them |

Exit code `3` is the one to watch for. `get` and `ls` also print `Sign-in expired for <host>. Run ampsync login ... , then run the same command again to resume.` to standard error. With `--json`, `get` also prints an `auth_expired` event.

## When the session expires

SharePoint sign-in cookies last from hours to days, depending on your organisation's policy. When `ampsync get` or `ls` reports that the sign-in expired (exit code `3`), repeat the steps above with a fresh cookie: sign in in a browser, copy the `Cookie` header, run `ampsync login ... --cookie-file ...`, then run the same `get` command. Nothing already downloaded is lost, and `get` continues where it stopped. A container cannot renew the session by itself, because it has no browser to sign in with.

For a long-running pipeline, plan for this: have the job stop on exit code `3`, alert someone, and rerun after the cookie has been refreshed.

## Troubleshooting

**The command stops at the prompt and shows `[1]+ Stopped`.** The link was not quoted and its `&` sent the command to the background. Quote the link: `'...'`. See [Quote the URL](#quote-the-url).

**"Can't ask for a pasted cookie without a terminal."** `ampsync login` wanted to ask you to paste the cookie, but the container has no terminal. Give it the cookie with `--cookie-file cookie.txt`, or `--cookie-file -` with the cookie on standard input.

**An error about the keychain, the Secret Service or D-Bus.** The container has no OS keychain. Set `AMPSYNC_SESSION_STORE=file` for both `login` and `get`, and keep the same `AMPSYNC_DATA_DIR` for both.

**"You're not signed in to <host>."** The `get` command cannot find a session. Check that `login` ran with the same `AMPSYNC_SESSION_STORE` and `AMPSYNC_DATA_DIR`, and that the data folder is on a volume that was kept.

**"Sign-in expired" and exit code 3.** The cookie is no longer valid. Sign in again with a fresh cookie, then rerun the same command.

## Removing stored data

```bash
ampsync sessions                     # list stored sign-ins
ampsync logout contoso.sharepoint.com  # forget one host
ampsync purge --yes                  # delete every sign-in, the settings and all app data
```

Run these with the same `AMPSYNC_SESSION_STORE` and `AMPSYNC_DATA_DIR` (and the same volume) as the other commands. Without a terminal, `purge` needs `--yes`. Your downloaded files are never touched. If you no longer need the volume, remove it too: `docker volume rm ampsync-data`. Also delete any cookie file you saved.
