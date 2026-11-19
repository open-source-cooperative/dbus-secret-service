// Copyright 2016-2024 dbus-secret-service Contributors
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! A session bus connection whose setup has a time limit.
//!
//! `Connection::new_session` calls libdbus `dbus_bus_get_private`, which
//! waits without limit for the bus to finish authentication and answer
//! `Hello`, and holds a process-wide libdbus lock while it waits.  Here we
//! open the socket with `dbus_connection_open_private` (no global lock),
//! queue our own `Hello`, and drive the connection until the `Hello` reply
//! arrives or the deadline passes.
//!
//! We wait for the socket with our own `poll` (see `wait_and_read_write`),
//! because libdbus restarts a `poll` that a signal interrupted with its whole
//! timeout, so a thread that gets signals often would never time out.

use std::ffi::OsString;
use std::time::{Duration, Instant};

use dbus::blocking::Connection;
use dbus::channel::Channel;
use dbus::{Error, Message};

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const ERROR_TIMEOUT: &str = "org.freedesktop.DBus.Error.Timeout";
const ERROR_DISCONNECTED: &str = "org.freedesktop.DBus.Error.Disconnected";
const ERROR_INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const ERROR_NO_MEMORY: &str = "org.freedesktop.DBus.Error.NoMemory";

/// Connect to the session bus, limiting authentication and `Hello` to `timeout`.
pub(crate) fn session_connection(timeout: Duration) -> Result<Connection, Error> {
    // libdbus ignores the environment in setuid processes; let it decide.
    if is_setuid() {
        return Connection::new_session();
    }
    match lookup_session_address(|name| std::env::var_os(name), current_uid()) {
        Some(address) => connect_address(&address, timeout),
        // libdbus uses its built-in default (autolaunch: or launchd).
        None => Connection::new_session(),
    }
}

/// Open `address`, authenticate and say `Hello`, giving up after `timeout`.
pub(crate) fn connect_address(address: &str, timeout: Duration) -> Result<Connection, Error> {
    // socket() and connect(); libdbus starts authentication but takes no global lock.
    let mut channel = Channel::open_private(address)?;
    // So that wait_and_read_write can ask what libdbus waits for.
    channel.set_watch_enabled(true);
    let deadline = Deadline::new(timeout);
    let hello = Message::method_call(
        &BUS_NAME.into(),
        &BUS_PATH.into(),
        &BUS_NAME.into(),
        &"Hello".into(),
    );
    // libdbus does not write the queued Hello until authentication is done,
    // so an empty outgoing queue means we are authenticated and Hello is sent.
    let serial = channel
        .send(hello)
        .map_err(|()| Error::new_custom(ERROR_NO_MEMORY, "Failed to queue Hello"))?;
    while channel.has_messages_to_send() {
        let wait = deadline.remaining("authentication")?;
        // This fails exactly when the connection is closed.
        if wait_and_read_write(&channel, wait).is_err() {
            return Err(disconnected("authentication"));
        }
    }
    loop {
        while let Some(mut message) = channel.pop_message() {
            if message.get_reply_serial() == Some(serial) {
                // An error reply is reported as is, as dbus_bus_register does.
                message.as_result()?;
                message.read1::<&str>().map_err(|e| {
                    Error::new_custom(ERROR_INVALID_ARGS, &format!("Bad Hello reply: {e}"))
                })?;
                // As from Connection::new_session: no watch tracking.
                channel.set_watch_enabled(false);
                return Ok(Connection::from(channel));
            }
            // A conforming bus sends us nothing else before the Hello reply.
        }
        let wait = deadline.remaining("the reply to Hello")?;
        if wait_and_read_write(&channel, wait).is_err() {
            return Err(disconnected("Hello"));
        }
    }
}

/// Wait at most `wait` for the socket to be ready for what libdbus wants to
/// do next, then let libdbus read and write without blocking.
///
/// A signal may end the wait early; the caller then checks its deadline
/// and calls again.  (`read_write(Some(wait))` would not return: libdbus
/// restarts an interrupted `poll` with the whole timeout.)
#[cfg(unix)]
fn wait_and_read_write(channel: &Channel, wait: Duration) -> Result<(), ()> {
    // libdbus closes the socket when it disconnects, and the watch then
    // still names its (maybe reused) fd.
    if !channel.is_connected() {
        return Err(());
    }
    let watch = channel.watch();
    let mut events = 0;
    if watch.read {
        events |= libc::POLLIN;
    }
    if watch.write {
        events |= libc::POLLOUT;
    }
    let mut fd = libc::pollfd {
        fd: watch.fd,
        events,
        revents: 0,
    };
    // The result does not matter: read_write finds out what is ready, and
    // that the socket is closed.  `wait` is at most i32::MAX ms.
    // SAFETY: `fd` is a valid pollfd and we pass a count of 1.
    unsafe { libc::poll(&mut fd, 1, wait.as_millis() as libc::c_int) };
    channel.read_write(Some(Duration::ZERO))
}

#[cfg(not(unix))]
fn wait_and_read_write(channel: &Channel, wait: Duration) -> Result<(), ()> {
    channel.read_write(Some(wait))
}

fn disconnected(during: &str) -> Error {
    Error::new_custom(
        ERROR_DISCONNECTED,
        &format!("The bus closed the connection during {during}"),
    )
}

struct Deadline {
    timeout: Duration,
    // None if `now + timeout` overflows: there is no end.
    at: Option<Instant>,
}

impl Deadline {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            at: Instant::now().checked_add(timeout),
        }
    }

    /// How long the next wait may block: see [`wait_millis`].
    fn remaining(&self, waiting_for: &str) -> Result<Duration, Error> {
        let left = match self.at {
            Some(at) => at.saturating_duration_since(Instant::now()),
            None => Duration::MAX,
        };
        if left.is_zero() {
            return Err(Error::new_custom(
                ERROR_TIMEOUT,
                &format!(
                    "Timed out after {:?} waiting for {waiting_for}",
                    self.timeout
                ),
            ));
        }
        Ok(wait_millis(left))
    }
}

/// `left` rounded up to whole milliseconds (never a 0 ms poll) and at most
/// `i32::MAX` ms, because `poll` (or dbus-rs, for libdbus) takes it as a C
/// `int` of milliseconds.
fn wait_millis(left: Duration) -> Duration {
    let millis = left.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as u64;
    Duration::from_millis(millis)
}

/// Find the session bus address the way libdbus does (`init_session_address`
/// and `_dbus_lookup_user_bus`), except for the built-in default, for which
/// this returns `None`.
fn lookup_session_address(getenv: impl Fn(&str) -> Option<OsString>, uid: u32) -> Option<String> {
    // An empty value counts as unset.
    if let Some(address) = getenv("DBUS_SESSION_BUS_ADDRESS") {
        if !address.is_empty() {
            return Some(address.to_string_lossy().into_owned());
        }
    }
    // On macOS, libdbus asks launchd instead of looking in XDG_RUNTIME_DIR.
    if cfg!(target_os = "macos") {
        return None;
    }
    user_bus_address(getenv("XDG_RUNTIME_DIR")?, uid)
}

/// `$XDG_RUNTIME_DIR/bus`, if it is a socket owned by `uid`.
#[cfg(unix)]
fn user_bus_address(runtime_dir: OsString, uid: u32) -> Option<String> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let mut path = runtime_dir.into_vec();
    path.extend_from_slice(b"/bus");
    let metadata = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(&path)).ok()?;
    if metadata.uid() != uid || !metadata.file_type().is_socket() {
        return None;
    }
    Some(format!("unix:path={}", escape_address_value(&path)))
}

#[cfg(not(unix))]
fn user_bus_address(_runtime_dir: OsString, _uid: u32) -> Option<String> {
    None
}

/// Escape a value for a D-Bus address, as libdbus `_dbus_address_append_escaped` does.
#[cfg(unix)]
fn escape_address_value(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"-_/\\*.".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02x}"));
        }
    }
    escaped
}

/// Whether libdbus may ignore the environment because the process is
/// setuid (`_dbus_check_setuid`). This may say yes when libdbus would say
/// no, which only means `new_session` is used, but never the other way.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn is_setuid() -> bool {
    // libdbus built with musl (Alpine) uses issetugid(), which musl answers
    // from these exec-time values in the auxiliary vector.
    // SAFETY: getauxval has no preconditions and cannot fail.
    let secure_exec = unsafe {
        libc::getauxval(libc::AT_SECURE) != 0
            || libc::getauxval(libc::AT_UID) != libc::getauxval(libc::AT_EUID)
            || libc::getauxval(libc::AT_GID) != libc::getauxval(libc::AT_EGID)
    };
    // libdbus built with glibc compares the real, effective and saved IDs.
    let (mut ruid, mut euid, mut suid) = (0, 0, 0);
    let (mut rgid, mut egid, mut sgid) = (0, 0, 0);
    // SAFETY: getresuid and getresgid only write through the valid pointers given.
    let ok = unsafe {
        libc::getresuid(&mut ruid, &mut euid, &mut suid) == 0
            && libc::getresgid(&mut rgid, &mut egid, &mut sgid) == 0
    };
    if !ok {
        // SAFETY: these calls have no preconditions and cannot fail.
        unsafe {
            ruid = libc::getuid();
            euid = libc::geteuid();
            suid = ruid;
            rgid = libc::getgid();
            egid = libc::getegid();
            sgid = rgid;
        }
    }
    secure_exec || ruid != euid || ruid != suid || rgid != egid || rgid != sgid
}

/// Whether libdbus may ignore the environment because the process is
/// setuid (`_dbus_check_setuid`).
#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn is_setuid() -> bool {
    // libdbus uses issetugid() here. It stays true after a setuid process
    // changes or drops its IDs, so the ID comparison alone is not enough.
    // SAFETY: these calls have no preconditions and cannot fail.
    unsafe {
        libc::issetugid() != 0
            || libc::getuid() != libc::geteuid()
            || libc::getgid() != libc::getegid()
    }
}

/// Whether libdbus may ignore the environment because the process is
/// setuid (`_dbus_check_setuid`).
#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))
))]
fn is_setuid() -> bool {
    // We do not know which check libdbus uses here, so let it decide.
    true
}

#[cfg(not(unix))]
fn is_setuid() -> bool {
    false
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
#[cfg(unix)]
mod test {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    #[derive(Clone, Copy, Debug)]
    enum Bus {
        /// Accept the socket, never write.
        Silent,
        /// Finish authentication, never answer Hello.
        AuthOnly,
        /// Reject every authentication mechanism.
        Reject,
        /// Close the socket after authentication.
        Close,
        /// Answer Hello with an error.
        HelloError,
        /// Answer Hello with a body that is not a string.
        HelloBadBody,
        /// Answer Hello with ":1.42", then send NameAcquired.
        HelloOk,
    }

    static COUNT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dss-{}-{}-{name}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake bus on a unix socket.  It sends the member of each message it
    /// reads to the returned receiver.
    fn fake_bus(bus: Bus) -> (String, mpsc::Receiver<String>) {
        let path = temp_dir("bus").join("bus");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let tx = tx.clone();
                thread::spawn(move || serve(stream, bus, tx));
            }
        });
        let address = format!(
            "unix:path={}",
            escape_address_value(path.to_str().unwrap().as_bytes())
        );
        (address, rx)
    }

    fn serve(mut stream: UnixStream, bus: Bus, tx: mpsc::Sender<String>) {
        let mut buf = Vec::new();
        let mut authenticated = false;
        loop {
            while !authenticated {
                let Some(end) = buf.windows(2).position(|w| w == b"\r\n") else {
                    break;
                };
                let line: Vec<u8> = buf.drain(..end + 2).collect();
                // The client sends a NUL byte before its first line.
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_start_matches('\0');
                let reply: &[u8] = match bus {
                    Bus::Silent => b"",
                    Bus::Reject => b"REJECTED EXTERNAL\r\n",
                    _ if line.starts_with("AUTH") => b"OK 0123456789abcdef0123456789abcdef\r\n",
                    _ if line.starts_with("NEGOTIATE_UNIX_FD") => b"AGREE_UNIX_FD\r\n",
                    _ if line.starts_with("BEGIN") => {
                        if let Bus::Close = bus {
                            return;
                        }
                        authenticated = true;
                        b""
                    }
                    _ => b"ERROR\r\n",
                };
                if stream.write_all(reply).is_err() {
                    return;
                }
            }
            if authenticated {
                while let Ok(needed) = Message::demarshal_bytes_needed(&buf) {
                    if buf.len() < needed {
                        break;
                    }
                    let bytes: Vec<u8> = buf.drain(..needed).collect();
                    let message = Message::demarshal(&bytes).unwrap();
                    let member = message.member().unwrap().to_string();
                    let _ = tx.send(member.clone());
                    if member == "Hello" && stream.write_all(&answer_hello(bus, &message)).is_err()
                    {
                        return;
                    }
                }
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn answer_hello(bus: Bus, hello: &Message) -> Vec<u8> {
        let replies = match bus {
            Bus::HelloError => vec![hello.error(
                &"org.freedesktop.DBus.Error.AccessDenied".into(),
                c"go away",
            )],
            Bus::HelloBadBody => vec![hello.method_return().append1(7u32)],
            Bus::HelloOk => vec![
                hello.method_return().append1(":1.42"),
                Message::new_signal(BUS_PATH, BUS_NAME, "NameAcquired")
                    .unwrap()
                    .append1(":1.42"),
            ],
            _ => vec![],
        };
        let mut bytes = Vec::new();
        for (serial, mut reply) in (1..).zip(replies) {
            reply.set_serial(serial);
            reply
                .marshal(|b| -> Result<(), ()> {
                    bytes.extend_from_slice(b);
                    Ok(())
                })
                .unwrap();
        }
        bytes
    }

    /// Run `f` on another thread, failing (not hanging) if it takes over 10 seconds.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("did not return within 10 seconds")
    }

    fn connect(address: String, timeout: Duration) -> (Result<Connection, Error>, Duration) {
        bounded(move || {
            let start = Instant::now();
            let result = connect_address(&address, timeout);
            (result, start.elapsed())
        })
    }

    #[test]
    fn connect_errors() {
        let half = Duration::from_millis(500);
        let five = Duration::from_secs(5);
        let cases = [
            (Bus::Silent, half, ERROR_TIMEOUT, "authentication"),
            (Bus::AuthOnly, half, ERROR_TIMEOUT, "Hello"),
            (Bus::Reject, five, ERROR_DISCONNECTED, "authentication"),
            (Bus::Close, five, ERROR_DISCONNECTED, ""),
            (
                Bus::HelloError,
                five,
                "org.freedesktop.DBus.Error.AccessDenied",
                "go away",
            ),
            (Bus::HelloBadBody, five, ERROR_INVALID_ARGS, "Hello"),
        ];
        for (bus, timeout, name, text) in cases {
            let (address, seen) = fake_bus(bus);
            let (result, elapsed) = connect(address, timeout);
            let Err(error) = result else {
                panic!("{bus:?}: connected");
            };
            assert_eq!(error.name(), Some(name), "{bus:?}: {error:?}");
            assert!(
                error.message().unwrap().contains(text),
                "{bus:?}: {error:?}"
            );
            if name == ERROR_TIMEOUT {
                assert!(elapsed >= timeout, "{bus:?}: {elapsed:?}");
                assert!(elapsed < Duration::from_secs(3), "{bus:?}: {elapsed:?}");
            } else {
                assert!(elapsed < Duration::from_secs(2), "{bus:?}: {elapsed:?}");
            }
            if let Bus::AuthOnly | Bus::HelloError | Bus::HelloBadBody = bus {
                assert_eq!(seen.try_iter().collect::<Vec<_>>(), ["Hello"], "{bus:?}");
            }
        }
    }

    #[test]
    fn hello_ok_returns_connection() {
        let (address, seen) = fake_bus(Bus::HelloOk);
        let (result, elapsed) = connect(address, Duration::from_secs(5));
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        let connection = result.unwrap();
        let channel = connection.channel();
        assert!(channel.is_connected());
        // NameAcquired, sent right after the reply, is still there for the caller.
        let start = Instant::now();
        let mut acquired = None;
        while acquired.is_none() && start.elapsed() < Duration::from_secs(2) {
            channel.read_write(Some(Duration::from_millis(50))).unwrap();
            while let Some(message) = channel.pop_message() {
                if message.member().as_deref() == Some("NameAcquired") {
                    acquired = Some(message.read1::<String>().unwrap());
                }
            }
        }
        assert_eq!(acquired.as_deref(), Some(":1.42"));
        assert_eq!(seen.try_iter().collect::<Vec<_>>(), ["Hello"]);
    }

    #[test]
    fn huge_and_zero_timeouts() {
        let (address, _) = fake_bus(Bus::HelloOk);
        assert!(connect(address, Duration::MAX).0.is_ok());
        let (address, _) = fake_bus(Bus::Silent);
        let (result, elapsed) = connect(address, Duration::ZERO);
        assert_eq!(result.err().unwrap().name(), Some(ERROR_TIMEOUT));
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");

        let most = Duration::from_millis(i32::MAX as u64);
        assert_eq!(Deadline::new(Duration::MAX).remaining("x").unwrap(), most);
        let far = Deadline::new(Duration::from_secs(u64::MAX / 2));
        assert_eq!(far.remaining("x").unwrap(), most);
        assert_eq!(wait_millis(Duration::MAX), most);
        assert_eq!(
            wait_millis(Duration::from_micros(10)),
            Duration::from_millis(1)
        );
        assert_eq!(
            wait_millis(Duration::from_millis(7)),
            Duration::from_millis(7)
        );
    }

    /// While one thread waits for a silent bus, another can still do what
    /// needs a libdbus global lock: `dbus_bus_register` takes
    /// `_DBUS_LOCK(bus_datas)`.
    #[test]
    fn waiting_holds_no_global_lock() {
        let (silent, _) = fake_bus(Bus::Silent);
        let (ok, _) = fake_bus(Bus::HelloOk);
        let (done, waiting) = mpsc::channel();
        thread::spawn(move || {
            let _ = done.send(connect_address(&silent, Duration::from_secs(3)));
        });
        thread::sleep(Duration::from_millis(300));
        let took = bounded(move || {
            let start = Instant::now();
            let mut channel = Channel::open_private(&ok).unwrap();
            channel.register().unwrap();
            assert_eq!(channel.unique_name(), Some(":1.42"));
            drop(channel);
            start.elapsed()
        });
        assert!(took < Duration::from_secs(2), "{took:?}");
        let error = waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("did not return within 10 seconds")
            .err()
            .unwrap();
        assert_eq!(error.name(), Some(ERROR_TIMEOUT));
    }

    /// libdbus restarts an interrupted `poll` with its whole timeout, so a
    /// thread that gets signals more often than that must not wait in libdbus.
    #[test]
    fn signals_do_not_extend_the_timeout() {
        use std::os::unix::thread::JoinHandleExt;

        extern "C" fn ignore(_: libc::c_int) {}
        // SIGURG is ignored by default and nothing else in the tests sends
        // it, so a handler for it changes nothing else.  Without
        // SA_RESTART, a signal makes the blocked poll fail with EINTR.
        static HANDLER: std::sync::Once = std::sync::Once::new();
        HANDLER.call_once(|| {
            // SAFETY: a zeroed sigaction with an empty mask and a handler
            // that does nothing is valid.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = ignore as *const () as libc::sighandler_t;
                libc::sigemptyset(&mut action.sa_mask);
                assert_eq!(
                    libc::sigaction(libc::SIGURG, &action, std::ptr::null_mut()),
                    0
                );
            }
        });
        let timeout = Duration::from_millis(200);
        for (bus, waiting_for) in [(Bus::Silent, "authentication"), (Bus::AuthOnly, "Hello")] {
            let (address, _) = fake_bus(bus);
            let start = Instant::now();
            let connecting = thread::spawn(move || connect_address(&address, timeout));
            // Signal every 10 ms, for at most 3 seconds.
            while !connecting.is_finished() && start.elapsed() < Duration::from_secs(3) {
                // std and libc do not agree on the type of pthread_t on musl.
                let pthread = connecting.as_pthread_t() as libc::pthread_t;
                // SAFETY: the thread is not joined yet, so its pthread_t is valid.
                unsafe { libc::pthread_kill(pthread, libc::SIGURG) };
                thread::sleep(Duration::from_millis(10));
            }
            let elapsed = start.elapsed();
            // Before join, which waits as long as the thread does.
            assert!(
                elapsed < Duration::from_millis(1500),
                "{bus:?}: {elapsed:?}"
            );
            let error = connecting.join().unwrap().err().unwrap();
            assert_eq!(error.name(), Some(ERROR_TIMEOUT), "{bus:?}: {error:?}");
            assert!(
                error.message().unwrap().contains(waiting_for),
                "{bus:?}: {error:?}"
            );
        }
    }

    #[test]
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn tests_are_not_setuid() {
        // Otherwise session_connection would fall back to new_session.
        assert!(!is_setuid());
    }

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn lookup_prefers_session_bus_address() {
        let dir = temp_dir("env");
        let _listener = UnixListener::bind(dir.join("bus")).unwrap();
        let vars = [
            ("DBUS_SESSION_BUS_ADDRESS", "tcp:host=x"),
            ("XDG_RUNTIME_DIR", dir.to_str().unwrap()),
        ];
        assert_eq!(
            lookup_session_address(env(&vars), current_uid()).as_deref(),
            Some("tcp:host=x")
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn lookup_uses_user_bus() {
        let dir = temp_dir("x d,g=");
        let _listener = UnixListener::bind(dir.join("bus")).unwrap();
        let vars = [
            ("DBUS_SESSION_BUS_ADDRESS", ""),
            ("XDG_RUNTIME_DIR", dir.to_str().unwrap()),
        ];
        let address = lookup_session_address(env(&vars), current_uid()).unwrap();
        let path = format!("{}/bus", dir.to_str().unwrap());
        let expected = format!("unix:path={}", escape_address_value(path.as_bytes()));
        assert_eq!(address, expected);
        assert!(address.contains("x%20d%2cg%3d/bus"), "{address}");
    }

    #[test]
    fn lookup_skips_unusable_user_bus() {
        let uid = current_uid();
        let file = temp_dir("file");
        std::fs::write(file.join("bus"), b"").unwrap();
        let vars = [("XDG_RUNTIME_DIR", file.to_str().unwrap())];
        assert_eq!(lookup_session_address(env(&vars), uid), None);

        let foreign = temp_dir("foreign");
        let _listener = UnixListener::bind(foreign.join("bus")).unwrap();
        let vars = [("XDG_RUNTIME_DIR", foreign.to_str().unwrap())];
        assert_eq!(lookup_session_address(env(&vars), uid + 1), None);

        let missing = temp_dir("missing");
        let vars = [("XDG_RUNTIME_DIR", missing.to_str().unwrap())];
        assert_eq!(lookup_session_address(env(&vars), uid), None);

        assert_eq!(lookup_session_address(env(&[]), uid), None);
    }

    #[test]
    fn escape_matches_libdbus() {
        assert_eq!(
            escape_address_value(b"/run/user/1000/bus"),
            "/run/user/1000/bus"
        );
        assert_eq!(
            escape_address_value(b"a b,c=d;e\\f*g.h-i_j\xff"),
            "a%20b%2cc%3dd%3be\\f*g.h-i_j%ff"
        );
    }
}
