# Contributing to Nanofy

Thanks for wanting to help! Bug reports, ideas and pull requests are all welcome, **in English or
Spanish** (*también en español*).

- **Found a bug?** Open an [issue](https://github.com/ElRobaMichis/nanofy/issues/new/choose) with the bug template.
- **Have an idea?** Open a feature request first, so we can agree on it before you spend time coding.
- **Want to translate?** Each language is one CSV file; see [lang/README.md](lang/README.md).
- **Want to code?** Look for issues labeled
  [`good first issue`](https://github.com/ElRobaMichis/nanofy/labels/good%20first%20issue) or
  [`help wanted`](https://github.com/ElRobaMichis/nanofy/labels/help%20wanted), and say in the issue
  that you're on it.

Please read the [code of conduct](CODE_OF_CONDUCT.md). Security problems go
[privately](SECURITY.md), not in issues.

## Reporting a bug

Include the version (Settings → About Nanofy), your system and the steps to reproduce it. Attach
`nanofy.log` (current session) and `nanofy.log.1` (the previous one), taken right after the problem:
they're in `%LOCALAPPDATA%\nanofy\data` (Windows), `~/.local/state/nanofy` (Linux) or
`~/Library/Application Support/nanofy` (macOS). The logs include your Spotify username and may include
song titles; review them before posting.

## Building and testing

The requirements are in the [README](README.md#build-from-source). Then:

```sh
cargo build                     # quick debug build (target/debug/nanofy)
cargo test --bin nanofy         # the unit tests (what CI runs)
cargo build --release           # optimized build, as released (slower: fat LTO)
```

`--bin nanofy` also runs the tests of the patched modules in `vendor/` (they're included with
`#[path]`), but not those of the vendored crates themselves (`librespot-core`'s connect to Spotify).

CI ([`ci.yml`](.github/workflows/ci.yml)) runs on every push to `main` and every pull request: the
unit tests on Windows, and a compile check on Linux and macOS. Changes that only touch documentation
skip it.

### Windows without admin rights

Use the GNU toolchain with WinLibs' gcc:

```powershell
Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe -OutFile rustup-init.exe
.\rustup-init.exe -y --default-host x86_64-pc-windows-gnu --profile minimal
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
.\scripts\build.cmd
```

`scripts\build.cmd` adds the folder where winget installs WinLibs to the PATH, builds and opens
Nanofy. `scripts/build.sh` builds on Linux, macOS or Git Bash without opening it.

## Pull requests

1. Fork the repo and create a branch from `main` with a short descriptive name:
   `fix/lyrics-offset`, `feat/tray-icon`, `docs/readme-linux`.
2. Keep each pull request focused on one thing. Small PRs get reviewed and merged much faster.
3. Make sure `cargo test --bin nanofy` passes and try the change in the app.
4. Open the pull request against `main` and fill in the template. For visual changes, add before and
   after screenshots.

Pull requests are **squash-merged**, so the commits inside your branch don't need to be perfect; the
PR title becomes the commit message.

### Style

- Follow the style of the file you're changing. CI doesn't run rustfmt or clippy (the existing code
  isn't formatted with them, and a mass reformat would bury real changes).
- Code comments, log messages and commit messages in this repo are mostly in **Spanish**, and the
  interface is in Spanish. You can write yours in English; nobody will ask you to translate.
- Comments explain *why*, not *what*. If something exists because of a specific Spotify quirk, say so.
- New user-visible text goes in Spanish, matching the rest of the interface. If you don't speak
  Spanish, write it in English and mention it in the PR; it'll be translated before merging.

### Platforms

CI only tests on Windows; Linux and macOS are compile-checked. If your change affects Linux or macOS
behavior, say in the PR how you tested it.

### Changes to `vendor/`

`vendor/` holds `librespot-core`, `librespot-connect` and `librespot-playback` 0.8 with Nanofy's own
patches (`[patch.crates-io]` in `Cargo.toml`). If you change something there, explain in the PR what it
fixes compared to upstream librespot, and add a short comment in the code saying why.

### License

By contributing you agree that your code is published under the project's [MIT license](LICENSE).

## Project layout

| Path | What's there |
|---|---|
| `src/main.rs`, `src/bus.rs` | Startup, logging (`nanofy.log`) and the message channel to the UI |
| `src/app/` | The interface (egui): state, pages, panels, player bar, song page |
| `src/shell.rs`, `src/raster.rs` | The window (winit + softbuffer) and CPU rasterization |
| `src/backend.rs`, `src/ap_keeper.rs` | librespot session, Spotify Connect device, player, and keeping the connection alive |
| `src/api.rs`, `src/pathfinder.rs` | Web API and internal endpoints, search |
| `src/lyrics_sources.rs`, `src/genius.rs` | Lyrics formats and Genius annotations |
| `src/model.rs` | Data models |
| `src/webauth.rs` | Library authorization (OAuth with PKCE) |
| `src/config.rs`, `src/cache.rs` | Settings, paths and the local copy of the library |
| `src/images.rs`, `src/fonts.rs` | Cover art and fonts |
| `src/media.rs`, `src/taskbar.rs` | Media keys and system controls; taskbar thumbnail buttons (Windows) |
| `src/update.rs` | Checking, downloading, installing and rolling back updates |
| `src/control.rs` | Local control mode for automated testing (`--control <port>`, only on `127.0.0.1`) |
| `vendor/` | Patched librespot crates (see above) |
| `assets/` | Icon (see [`assets/README.md`](assets/README.md)) |
| `docs/screenshots/` | Screenshots used in the README |
| `packaging/` | Files that go inside the release zips (`LEEME.txt`) |
| `scripts/` | Local build and packaging helpers |
| `.github/` | CI, release workflow, issue and PR templates |

## Branches and releases

- `main` is always releasable. Work happens in short-lived branches that are merged through pull
  requests.
- Every fix or feature bumps the version in `Cargo.toml` following [SemVer](https://semver.org)
  (`MAJOR.MINOR.PATCH`). The maintainer does this when releasing; you don't need to touch it in your PR.

### Publishing a release (maintainers)

1. Bump `version` in `Cargo.toml`, build so `Cargo.lock` is updated, and commit both.
2. Create an annotated tag with that version (its message becomes the release notes, after a fixed line
   for users on 1.3 or older) and push it with `main`:
   ```sh
   git tag -a vX.Y.Z -m "What changes in this version"
   git push origin main vX.Y.Z
   ```
3. [`release.yml`](.github/workflows/release.yml) builds for Windows (MSVC), Linux and macOS (Intel and
   Apple Silicon), checks the size and SHA-256 of what it uploaded and publishes the zips with
   `SHA256SUMS.txt`. It can also be run by hand from Actions, with the tag (or empty, to only build).

- If the tag doesn't match the version in `Cargo.toml`, nothing is published.
- The Windows and Linux zips are required; if one of the macOS builds fails, the release goes out without it.
- Don't rename the zips or upload other files to the release: the updater picks the zip by name, and
  the workflow fails if it finds anything else.
- A tag with a suffix (`vX.Y.Z-rc.N`) is published as a pre-release: it doesn't reach automatic updates,
  so it's useful for testing first.
- `NANOFY_CLIENT_ID` is an optional repository secret: if set, it's built in as the default Client ID of
  the optional personal developer app (Settings → Biblioteca → Avanzado). Signing in doesn't need it.

`scripts\dist.cmd` builds a Windows-only zip locally (`dist\Nanofy-<version>-windows-x64.zip`, with the
GNU toolchain); it doesn't replace the release.
