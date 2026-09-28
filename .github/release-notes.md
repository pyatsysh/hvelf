hvelf desktop installers for Linux x86_64, Windows x86_64, and macOS Intel and Apple Silicon.

This is a draft prerelease. Automated builds and tests do not establish desktop readiness. Linux still needs native desktop acceptance, including restart persistence and desktop integration. Windows and macOS behaviour also needs a manual regression check.

The Linux `.deb` targets Ubuntu 24.04 or a compatible newer system with WebKitGTK 4.1. Windows packages are unsigned NSIS installers. macOS disk images are not notarised. No signing credentials or updater keys are configured.

`SHA256SUMS` covers every installer and its `.json` build receipt. Receipts identify the source commit, platform and package hash.
