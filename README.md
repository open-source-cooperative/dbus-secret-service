# dbus-secret-service

This crate is a knock-off of the
[hwchen/secret-service](https://crates.io/crates/secret-service)
crate, which is currently at version 4
and uses
[zbus](https://crates.io/crates/zbus)
to access the secret service. The basic
collection, item and search APIs in this
crate are meant to work the same as the
blocking APIs in the zbus-based crate.
If they don't, please file a bug.

Why do a knock-off? So that folks who write
synchronous Rust apps that access the secret
service (typically through the
[hwchen/keyring](https://crates.io/crates/keyring)
crate) are not required to add an async
runtime. Because this knock-off uses lib-dbus,
it doesn't require an async runtime.

Why is this crate starting at version 4?
Since its API matches a particular
version of the dbus-based crate, I figured
it would be clearest if its version
number was matched that version as well.

## Usage

Just in case it wasn't clear from the above,
in order to use this crate on a given machine,
you will need to have `libdbus` installed.
Most do, but if yours doesn't then
search your package manager for `dbus`.

For dependency info, see this crate on
[crates.io](https://crates.io/crates/dbus-secret-service).
For code usage examples, see the
[documentation](https://docs.rs/crate/dbus-secret-service).

This crate has no default features, and requires
no features to run. If you need your secrets
to be encrypted on their way to and from the
secret service, then add one of the crypto features:

* `crypto-rust` uses pure Rust crates for encryption.
* `crypto-openssl` uses the openssl libraries for encryption (which must be installed).

See the
[documentation](https://docs.rs/crate/dbus-secret-service)
for details on how to specify use of an encrypted session.

Note: To build a project that uses this crate, your development machine
will need to have the dbus development headers installed. If your project
uses the `crypto-openssl` feature, you will also need to have the openssl
development headers installed.

### Functionality

- SecretService: initialize dbus, create plain/encrypted session.
- Collections: create, delete, search.
- Items: create, delete, search, get/set secret.

## Changelog

v4.0.0: first release, same API as secret-service v4.0.

## License

The copyright to all material in this repository belongs to
the collective of contributors who have checked material in to
this repository.

All material is this repository is licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise,
any contribution intentionally submitted
for inclusion in the work by you,
as defined in the Apache-2.0 license,
shall be dual licensed as above,
without any additional terms or conditions.
