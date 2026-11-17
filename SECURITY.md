# Security policy

## Reporting a vulnerability

Please report security problems privately, using GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability** (a GitHub security advisory). Do not open a public issue or pull request for it.

Include what you found, the version and operating system, and the steps to reproduce it. We will acknowledge your report, keep you informed while we investigate, and credit you in the advisory if you wish.

## Never share secrets

AmpSync works with a SharePoint browser session, which is as sensitive as a password.

- **Never paste cookies, `Cookie` headers, `FedAuth` or `rtFa` values, tokens or keychain contents** into an issue, pull request, comment or advisory. If you already did, sign out of the site in AmpSync, sign out of SharePoint in your browser, and delete the post.
- Remove SharePoint links, tenant and site names and file names from logs and screenshots before sharing them. The "Copy diagnostics" text in the app is meant to be safe to paste; check it anyway.

## Browser link trust boundaries

The optional browser link passes SharePoint sign-in cookies from an extension in your own browser to AmpSync. It stays off until you set it up. These are the limits it works within:

- **One extension ID.** AmpSync's native messaging host accepts only the ID of the AmpSync extension (`foohiaphdliphaogfjlahdmbjemmaocp`). The host refuses to start for any other origin.
- **Local and per user.** The host reaches the running app through a named pipe that only your Windows account can open, or on Ubuntu a Unix socket with mode `0600`. Remote clients are refused, so other accounts on the computer cannot connect.
- **The host holds no secrets.** The host only forwards messages. It never reads the keychain. AmpSync validates each session and stores it the same way as any other sign-in.
- **Strict validation.** A session is kept only for a SharePoint host (`.sharepoint.com`, `.us`, `.cn` or `.de`). Only cookies for that host or its parent SharePoint domain are kept, and all other cookies are dropped. `FedAuth` and `rtFa` must both be present. Message sizes, cookie counts and cookie value sizes are capped.
- **Known sites only.** AmpSync keeps a session pushed by the extension only for sites it already has a saved sign-in for, or that a queued download uses.
- **Per-user registration.** The host is registered for your Windows user, or in your home folder on Ubuntu. Unlinking or removing all data takes the registration and the extension folder away.
- **No cookie values in logs.** Neither AmpSync nor the extension writes cookie values to logs.
- **Narrow extension.** The extension asks only for SharePoint domains, has no content scripts and reads only SharePoint cookies.

## Scope

In scope: how AmpSync stores, sends and deletes sessions; sign-in handling, including the browser link (its extension, native messaging host and local pipe or socket); writing files outside the chosen destination folder (path traversal); anything that makes AmpSync contact hosts other than the user's SharePoint tenant and GitHub; the release workflow and published artifacts.

Out of scope: weaknesses in SharePoint, Microsoft sign-in, or your operating system; issues that need an attacker who already controls your user account or machine.

## Supported versions

Only the latest release receives fixes.

## Verifying downloads

Each release includes `SHA256SUMS.txt`. Compare the checksum of the file you downloaded with the matching line. The Windows files are not code-signed yet, so SmartScreen will warn when you run them.
