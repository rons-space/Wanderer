# Security Policy

Wander(er) holds a user's photo library and a Telegram account credential, so a
vulnerability here is not an inconvenience. Reports are welcome and will be answered.

## Reporting a vulnerability

Report privately through GitHub's [private vulnerability
reporting](https://github.com/rons-space/Wanderer/security/advisories/new). Do not open
a public issue for anything that lets someone read another user's media, recover key
material, or act on their Telegram account.

Please include what you did, what happened, and what you expected. A proof of concept
helps, even a rough one. If a report is a duplicate or turns out not to be exploitable,
you will be told which and why.

Expect an acknowledgement within a week. Fixes ship on `main` through the normal
promotion flow, and the advisory is published once a release carrying the fix exists.

## Supported versions

This project is pre-1.0 and there is no long-term support branch. Only the latest
release receives fixes.

## What is already known

These are documented rather than reported, and are tracked in the open issues:

- Local files under `backup/` are plaintext at rest even in encrypted mode. Encryption
  applies to what leaves the machine, not to the local library.
- Decrypted plaintext is written to `%TEMP%` for thumbnails and the view cache and is
  not purged on lock ([#28](https://github.com/rons-space/Wanderer/issues/28)).
- The Telegram session key in `session.db` is stored unprotected, unlike the api_id and
  api_hash, which are DPAPI-wrapped ([#30](https://github.com/rons-space/Wanderer/issues/30)).
- The metadata index, including GPS coordinates, is stored in plaintext in `library.db`.
- Release installers are not code signed
  ([#25](https://github.com/rons-space/Wanderer/issues/25)).

## Installer provenance

Releases come only from
[`rons-space/Wanderer`](https://github.com/rons-space/Wanderer/releases). The installer
is unsigned today, so Windows will warn about an unknown publisher. An installer
obtained anywhere else did not come from this project.
