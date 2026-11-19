## v4.2.0
- add connection timeouts (thanks @Brooooooklyn)
- fix badges in README to reflect new repository location.
- update edition to 2024 and MSRV to 1.85
- update compatible dependencies to latest 1.85 versions.
- add lots of good workflows for cleanliness checking (thanks @LucaCappelletti94)

## v4.1.0
- expose dbus::Path fields on Items and Collections
- allow creation of Items and Collections from a remembered path
- update the MSRV to 1.78 (minimum for `Cargo.lock` version 4)
- make lifetimes explicit to avoid Clippy complaints

### v4.0.3
- doc updates only (no code changes)

### v4.0.2 API and doc fixes
- add `Sync` to the error API
- fix repository URL in `Cargo.toml`

# v4.0.0 first public release
- same API as v4 of the secret-service crate.
