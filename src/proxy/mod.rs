// Copyright 2016-2024 dbus-secret-service Contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

// The generated modules start with `use dbus;`. A reason or `expect` would need Rust 1.81.
#[allow(clippy::single_component_path_imports)]
pub(crate) mod collection;
#[allow(clippy::single_component_path_imports)]
pub(crate) mod item;
#[allow(clippy::single_component_path_imports)]
pub(crate) mod prompt;
#[allow(clippy::single_component_path_imports)]
pub(crate) mod service;

use dbus::blocking::{Connection, Proxy};
use std::time::Duration;

pub(crate) fn new_proxy<'a, 'b>(
    connection: &'b Connection,
    path: &'a str,
) -> Proxy<'a, &'b Connection> {
    connection.with_proxy(crate::ss::SS_DBUS_DEST, path, Duration::from_millis(2000))
}
