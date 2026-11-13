# Security

## Reporting a vulnerability

Please don't open a public issue for security problems. Report them privately instead:

- GitHub: [Report a vulnerability](https://github.com/idlebench/perch/security/advisories/new)
- Email: idlebenchhq@gmail.com

You'll get a reply within a few days. Once it's fixed, the fix ships through the auto-updater and you'll be
credited in the release notes if you'd like.

## Supported versions

Only the latest release gets fixes. Perch updates itself, so most people are on it already.

## How updates are protected

Every update is signed with a private key that is never stored in this repository or on GitHub's servers.
Installed copies check the signature against the public key built into the app and refuse anything that doesn't
match.

## What Perch can see

Perch reads what's playing, the clipboard (in memory only, never written to disk, and never items that password
managers mark as private), files you drop on it, and the cursor position. It makes no network requests except
the update check against GitHub. Settings are the only thing saved to disk.
