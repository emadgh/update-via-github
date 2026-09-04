# update-via-github

A small Windows-only Rust crate for self-updating desktop applications from GitHub Releases.

It centralizes:

- checking `releases/latest`
- SemVer-style version comparison, including prerelease precedence
- selecting a named `.exe` release asset
- download progress reporting
- executable sanity checks (`MZ` + minimum size)
- SHA-256 verification using GitHub's release-asset `digest` field and/or a separate checksum asset
- staging the downloaded executable in `%TEMP%`
- safely replacing the running executable through a hidden PowerShell helper after the app exits

## Dependency

```toml
update-via-github = { git = "https://github.com/emadgh/update-via-github.git" }
```

## Managed updater

```rust
use update_via_github::{UpdateConfig, UpdateManager};

let config = UpdateConfig::new(
    "owner/my-app",
    "MyApp.exe",
    env!("CARGO_PKG_VERSION"),
)
.with_app_name("MyApp")
.with_max_download_size(50 * 1024 * 1024);

let updater = UpdateManager::new(config);
updater.start_check(false);
```

`UpdateManager` exposes `status`, `start_check`, `start_download`, and `apply_ready`.

## Low-level adapter API

Applications that already have their own UI state machine or Win32 message loop can keep it and call:

- `check_latest_release`
- `download_update`
- `apply_update`

This keeps app-specific notification/UI code local while all GitHub, download, verification, and executable replacement logic stays in this crate.

## Integrity policy

If GitHub returns a `sha256:...` digest for the selected release asset, the crate verifies it automatically. A named checksum asset can also be configured with `with_checksum_asset`. Use `with_required_checksum(true)` when an update must never be installed without a verifiable SHA-256 digest.

## Platform

Windows only. The minimum supported Rust toolchain is 1.85.
