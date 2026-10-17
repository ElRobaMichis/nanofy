# Security policy

## Supported versions

Only the [latest release](https://github.com/ElRobaMichis/nanofy/releases/latest) gets fixes.
Nanofy updates itself (unless you turned it off in Settings), so most users are on it within a few hours of a release.

## Reporting a vulnerability

Please **don't open a public issue**. Report it privately through
[GitHub's security advisories](https://github.com/ElRobaMichis/nanofy/security/advisories/new),
in English or Spanish, with the steps to reproduce it and the version you tested.

You'll get an answer within a week. Once a fix is released, the advisory is published with credit to you,
unless you prefer to stay anonymous.

## Scope

Things that are especially worth reporting:

- The self-updater (`src/update.rs`): anything that could make Nanofy install a file that isn't the official release.
- The local servers: the OAuth callback during sign-in, and the `--control` test mode, which must only listen on `127.0.0.1`.
- Credentials and tokens being sent anywhere other than Spotify, or written somewhere other than Nanofy's data folder.

Credentials are stored unencrypted in Nanofy's data folder (see [Privacy and data](README.md#privacy-and-data)); that is a known limitation, not a vulnerability.
