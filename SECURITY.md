# Security policy

## Reporting a vulnerability

Please report security problems privately, using GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability** (a GitHub security advisory). Do not open a public issue or pull request for it.

Include what you found, the version and operating system, and the steps to reproduce it. We will acknowledge your report, keep you informed while we investigate, and credit you in the advisory if you wish.

## Never share secrets

AmpSync works with a SharePoint browser session, which is as sensitive as a password.

- **Never paste cookies, `Cookie` headers, `FedAuth` or `rtFa` values, tokens or keychain contents** into an issue, pull request, comment or advisory. If you already did, sign out of the site in AmpSync, sign out of SharePoint in your browser, and delete the post.
- Remove SharePoint links, tenant and site names and file names from logs and screenshots before sharing them. The "Copy diagnostics" text in the app is meant to be safe to paste; check it anyway.

## Scope

In scope: how AmpSync stores, sends and deletes sessions; sign-in handling; writing files outside the chosen destination folder (path traversal); anything that makes AmpSync contact hosts other than the user's SharePoint tenant and GitHub; the release workflow and published artifacts.

Out of scope: weaknesses in SharePoint, Microsoft sign-in, or your operating system; issues that need an attacker who already controls your user account or machine.

## Supported versions

Only the latest release receives fixes.

## Verifying downloads

Each release includes `SHA256SUMS.txt`. Compare the checksum of the file you downloaded with the matching line. The Windows files are not code-signed yet, so SmartScreen will warn when you run them.
