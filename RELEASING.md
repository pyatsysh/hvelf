# Building and releasing hvelf

The application code is shared by Linux, Windows and macOS. Linux desktop support is still being qualified. Build success, automated tests and interactive desktop acceptance are separate results.

## Reproduce a build

Use current stable Rust and Node.js 22. `Cargo.lock` and `package-lock.json` are committed; the Tauri CLI is pinned to 2.11.4. On Ubuntu 24.04 install:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf xdg-utils xvfb xauth dbus-x11
```

Windows needs the MSVC build tools and WebView2. macOS needs the Xcode command line tools. Follow the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for those platforms.

From the repository root:

```sh
npm ci --ignore-scripts --no-audit --no-fund
node --test .github/scripts/release.test.mjs
node .github/scripts/release.mjs check
cargo test --locked --manifest-path src-tauri/Cargo.toml
npm run build -- --ci --bundles deb -- --locked
```

Use `nsis` in place of `deb` on Windows, or `dmg` on macOS. Packages land under `src-tauri/target/release/bundle/`. A bare `cargo build --release --locked --manifest-path src-tauri/Cargo.toml` builds only the executable. Node is used for packaging and release checks; the static frontend has no compilation step.

For hvelf's X11 adapter, run a separate test against an isolated X server:

```sh
HVELF_X11_TEST=1 timeout 60s xvfb-run -a cargo test --locked --manifest-path src-tauri/Cargo.toml x11_ewmh_and_grab_against_a_real_server -- --nocapture
```

The ordinary test run does not exercise that test's X server path. A passing Xvfb check establishes protocol behaviour, not focus behaviour on a native desktop. Wayland portal binding is not implemented yet; use a desktop shortcut invoking `hvelf --toggle`.

## GitHub workflow

`.github/workflows/build.yml` follows the [Tauri packaging workflow](https://v2.tauri.app/distribute/pipelines/github/) and builds these targets:

| Runner | Installer |
|---|---|
| Ubuntu 24.04, x86_64 | `.deb` |
| Windows Server 2022, x86_64 | NSIS `.exe` |
| macOS 15, Intel | `.dmg` |
| macOS 15, Apple Silicon | `.dmg` |

Pull requests, pushes to `main`, and manual runs test and build every target. Installers and JSON build receipts are retained as workflow artifacts for 14 days. Build receipts record the commit and installer hash. The build jobs have read permission only. These workflow definitions were checked locally; successful hosted runs are still required.

To prepare a version, update `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` and `src-tauri/tauri.conf.json` together. The version check rejects disagreements and any tag that does not match `v<version>`. Commit the source through the normal repository process. Publishing that version tag starts the build; a manual run on an existing version tag can do the same.

Only after all four platform jobs pass does the release job verify their source commits and hashes, generate `SHA256SUMS`, and create a **draft prerelease**. It uses GitHub's workflow token and requires no additional service credentials. A normal branch build creates no release. An existing release is never overwritten by this workflow; a repeated release-creation attempt fails for review.

Review the draft's platform results and test the installers before publishing it. At present native Linux desktop acceptance remains pending, and Windows/macOS behaviour needs a regression check. Windows installers are unsigned and macOS disk images are not notarised. Signing, automatic updates and app-store distribution are not configured.
