use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::identity::{
    PaneAddress, monotonic_ns20, tty_fingerprint, tty_fingerprint_from_metadata,
};
use crate::protocol::{AttentionError, Result, Verdict, manifest, parse_wire_value};

pub trait Clock: Send + Sync {
    fn monotonic_ns20(&self) -> Result<String>;
    fn unix_ns20(&self) -> Result<String>;
}

pub trait TtyWriter: Send + Sync {
    fn current_path(&self) -> Result<String>;
    fn controlling_path(&self) -> Result<String> {
        self.current_path()
    }
    fn fingerprint(&self, path: &str) -> Result<String>;
    fn write(&self, path: &str, data: &[u8], expected_fingerprint: &str) -> Result<()>;
}

pub trait PaneLister: Send + Sync {
    fn list(&self, socket_path: &str) -> Result<Vec<PaneRow>>;
}

pub trait GuiWindowLister: Send + Sync {
    fn list_windows(&self, socket_path: &str) -> Result<BTreeSet<u64>>;
}

pub struct ExistingWeztermWindowLister;

impl GuiWindowLister for ExistingWeztermWindowLister {
    fn list_windows(&self, socket_path: &str) -> Result<BTreeSet<u64>> {
        parse_gui_window_ids(&list_wezterm_inventory(socket_path)?)
    }
}

pub fn parse_gui_window_ids(bytes: &[u8]) -> Result<BTreeSet<u64>> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(deserialize_with = "deserialize_pane_id")]
        pane_id: String,
        window_id: u64,
    }
    let invalid = || {
        AttentionError::new(
            "record_invalid",
            "GUI inventory contains invalid pane or window identities",
        )
    };
    if bytes.len() > manifest()?.limits.max_json_bytes {
        return Err(invalid());
    }
    let rows: Vec<Row> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let mut panes = BTreeSet::new();
    let mut windows = BTreeSet::new();
    for row in rows {
        crate::identity::canonical_pane_id(&row.pane_id).map_err(|_| invalid())?;
        if !panes.insert(row.pane_id) {
            return Err(invalid());
        }
        windows.insert(row.window_id);
    }
    Ok(windows)
}

pub trait ProcessProbe: Send + Sync {
    fn available(&self) -> bool;
    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence;
    /// Every socket and pane pair one process listing shows, so a caller with
    /// many panes to ask about can take the listing once.
    fn pane_processes(&self) -> ProcessListing {
        ProcessListing::NotOffered
    }
}

/// What a probe answers when asked for its whole listing.
///
/// The two empty-handed answers must stay apart: a probe that never lists is
/// asked one pane at a time, which is the only way it can answer; a probe
/// whose listing failed would answer a per-pane question by running the same
/// listing again, once per pane, so the caller stops asking instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessListing {
    /// This probe answers one pane at a time and offers no listing.
    NotOffered,
    /// This probe offers a listing and could not take one; asking it about a
    /// pane now would repeat that failure.
    Failed,
    Listed(PaneProcessSet),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    Present,
    /// No process of this user carries the pair, and every one was read.
    Absent,
    /// No process that could be read carries the pair, but some could not be
    /// read -- macOS hides the environment of its own system binaries -- so
    /// one that does may have been missed. This alone never shows a pane
    /// gone.
    Unseen,
    Unavailable,
}

/// The socket and pane pairs that live processes carry in their environment.
///
/// It is built from a process listing that holds every process's whole
/// environment, which is where API keys live. Only the two values this crate
/// looks for are kept, and the listing is dropped when parsing returns.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PaneProcessSet {
    pairs: BTreeSet<(String, String)>,
    /// Each socket spelling a process carries, as [`resolved_socket_path`]
    /// resolves it; `None` when its directory could not be resolved.
    resolved: BTreeMap<String, Option<PathBuf>>,
    /// Some process of this user was listed and its environment could not be
    /// read, so the pairs may be missing one.
    missed: bool,
}

/// A socket path as two spellings of it can be compared: its directory with
/// every symlink resolved, joined to its file name. The file itself is left
/// as named, because it may be the part that is gone. A realm record keeps
/// the path resolved, while a process keeps it as WezTerm was configured to
/// spell it, through `/tmp` on macOS or a symlinked home.
fn resolved_socket_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    let name = path.file_name()?;
    Some(fs::canonicalize(path.parent()?).ok()?.join(name))
}

impl PaneProcessSet {
    /// One line per process: its command followed by its environment.
    pub fn from_process_listing(listing: &str) -> Self {
        let mut processes = Self::default();
        for line in listing.lines() {
            let panes = environment_values(line, "WEZTERM_PANE=");
            if panes.is_empty() {
                continue;
            }
            for socket in environment_values(line, "WEZTERM_UNIX_SOCKET=") {
                for pane in &panes {
                    processes.insert(socket, pane);
                }
            }
        }
        processes
    }

    fn insert(&mut self, socket: &str, pane: &str) {
        if !self.resolved.contains_key(socket) {
            self.resolved
                .insert(socket.to_owned(), resolved_socket_path(socket));
        }
        self.pairs.insert((socket.to_owned(), pane.to_owned()));
    }

    /// Note that a process was listed whose environment could not be read.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    fn missed_one(&mut self) {
        self.missed = true;
    }

    /// Add the pairs one process's environment holds, given as `NAME=value`
    /// entries.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    fn add_environment<'a>(&mut self, entries: impl Iterator<Item = &'a [u8]>) {
        let mut sockets = Vec::new();
        let mut panes = Vec::new();
        for entry in entries {
            if let Some(value) = entry.strip_prefix(b"WEZTERM_UNIX_SOCKET=") {
                sockets.extend(std::str::from_utf8(value).ok());
            } else if let Some(value) = entry.strip_prefix(b"WEZTERM_PANE=") {
                panes.extend(std::str::from_utf8(value).ok());
            }
        }
        for socket in &sockets {
            for pane in &panes {
                self.insert(socket, pane);
            }
        }
    }

    /// Never `Unavailable`: a set that exists came from a listing that was read.
    ///
    /// A pair is seen when a process carries the pane id and the same socket,
    /// spelled the same or resolving to the same path. Not seeing it is
    /// `Absent` only when every process was read and every socket carrying
    /// that pane id could be compared; otherwise it is `Unseen`.
    pub fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        let wanted =
            resolved_socket_path(socket_path).unwrap_or_else(|| PathBuf::from(socket_path));
        let mut undecided = false;
        for (socket, _) in self.pairs.iter().filter(|(_, pane)| pane == pane_id) {
            if socket == socket_path {
                return Presence::Present;
            }
            match self.resolved.get(socket) {
                Some(Some(carried)) if *carried == wanted => return Presence::Present,
                Some(Some(_)) => {}
                _ => undecided = true,
            }
        }
        if undecided || self.missed {
            Presence::Unseen
        } else {
            Presence::Absent
        }
    }
}

pub struct RuntimePorts<'a> {
    pub clock: &'a dyn Clock,
    pub tty: &'a dyn TtyWriter,
    pub panes: &'a dyn PaneLister,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneRow {
    #[serde(deserialize_with = "deserialize_pane_id")]
    pub pane_id: String,
    pub tty_name: Option<String>,
}

fn deserialize_pane_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PaneId {
        String(String),
        Number(u64),
    }
    Ok(match PaneId::deserialize(deserializer)? {
        PaneId::String(value) => value,
        PaneId::Number(value) => value.to_string(),
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn monotonic_ns20(&self) -> Result<String> {
        monotonic_ns20()
    }

    fn unix_ns20(&self) -> Result<String> {
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AttentionError::new("clock_skew", "wall clock precedes Unix epoch"))?;
        Ok(format!("{:020}", duration.as_nanos()))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTtyWriter;

const TTY_WRITE_TIMEOUT: Duration = Duration::from_millis(250);

fn write_tty_with_deadline(file: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    let mut written = 0;
    let result = write_before(file, data, &mut written, Instant::now() + TTY_WRITE_TIMEOUT);
    if result.is_err() {
        let closing = cut_sequence_closing(data, written);
        if !closing.is_empty() {
            // Best effort, under a deadline of its own: a tty still stalled
            // takes nothing, and the sequence stays open as it would have.
            let mut closed = 0;
            let _ = write_before(
                file,
                closing,
                &mut closed,
                Instant::now() + TTY_WRITE_TIMEOUT,
            );
        }
    }
    result
}

/// Write `data[*written..]` until it is all written or `deadline` passes,
/// counting progress in `written` either way.
fn write_before(
    file: &mut impl Write,
    data: &[u8],
    written: &mut usize,
    deadline: Instant,
) -> std::io::Result<()> {
    while *written < data.len() {
        match file.write(&data[*written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "tty write made no progress",
                ));
            }
            Ok(count) => *written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "tty write timed out",
                    ));
                }
                thread::sleep((deadline - now).min(Duration::from_millis(5)));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// What to write after the first `written` bytes of a publication so the
/// terminal closes the sequence the cut left open without applying it.
///
/// A terminal applies an OSC when anything ends it -- BEL, ST, and in
/// WezTerm's parser also CAN, SUB or the next ESC, such as the colour codes
/// of the next prompt. Ended as it stands, a cut publication is applied
/// truncated, and a base64 value cut at a four-character boundary decodes to
/// a valid shorter value: pane 1234 would publish as pane 123. So a byte
/// that is not base64 goes first, which makes the whole sequence fail to
/// parse, and then BEL ends it. A cut right after ESC is completed as ST,
/// which does nothing. A cut between sequences needs nothing.
fn cut_sequence_closing(data: &[u8], written: usize) -> &'static [u8] {
    let sent = &data[..written.min(data.len())];
    let Some(start) = sent.iter().rposition(|byte| *byte == 0x1b) else {
        return b"";
    };
    if sent[start..].contains(&0x07) {
        b""
    } else if start + 1 == sent.len() {
        b"\\"
    } else {
        b"!\x07"
    }
}

impl SystemTtyWriter {
    pub fn validate_opened(file: &File, expected_fingerprint: &str) -> Result<()> {
        let metadata = file
            .metadata()
            .map_err(|_| AttentionError::new("unsafe_tty", "opened tty could not be inspected"))?;
        let actual = tty_fingerprint_from_metadata(&metadata)?;
        if actual != expected_fingerprint {
            return Err(AttentionError::new(
                "unsafe_tty",
                "opened tty identity does not match validation",
            ));
        }
        Ok(())
    }

    fn write_with_open(
        &self,
        path: &str,
        data: &[u8],
        expected_fingerprint: &str,
        open: impl FnOnce(&str) -> std::io::Result<File>,
    ) -> Result<()> {
        if tty_fingerprint(path)? != expected_fingerprint {
            return Err(AttentionError::new(
                "unsafe_tty",
                "tty identity changed before publication",
            ));
        }
        let mut file = open(path).map_err(|_| {
            AttentionError::new("unsafe_tty", "tty could not be opened for publication")
        })?;
        Self::validate_opened(&file, expected_fingerprint)?;
        write_tty_with_deadline(&mut file, data)
            .map_err(|_| AttentionError::new("unsafe_tty", "tty publication was incomplete"))
    }
}

impl TtyWriter for SystemTtyWriter {
    fn current_path(&self) -> Result<String> {
        tty_name_for_fd(libc::STDIN_FILENO)
    }

    fn controlling_path(&self) -> Result<String> {
        let terminal = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open("/dev/tty")
            .map_err(|_| {
                AttentionError::new("unsafe_tty", "controlling terminal is unavailable")
            })?;
        tty_name_for_fd(terminal.as_raw_fd())
    }

    fn fingerprint(&self, path: &str) -> Result<String> {
        tty_fingerprint(path)
    }

    fn write(&self, path: &str, data: &[u8], expected_fingerprint: &str) -> Result<()> {
        self.write_with_open(path, data, expected_fingerprint, |path| {
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)
        })
    }
}

fn tty_name_for_fd(fd: libc::c_int) -> Result<String> {
    ttyname(fd).ok_or_else(|| AttentionError::new("unsafe_tty", "stdin is not a terminal"))?
}

/// The terminal path behind `fd`, or `None` when it is not a terminal.
///
/// The buffer is bytes and only its pointer is cast, because `c_char` is `i8`
/// on some targets and `u8` on others (aarch64 Linux among them).
fn ttyname(fd: libc::c_int) -> Option<Result<String>> {
    let mut buffer = vec![0_u8; 4096];
    let result =
        unsafe { libc::ttyname_r(fd, buffer.as_mut_ptr().cast::<libc::c_char>(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    buffer.truncate(length);
    Some(
        String::from_utf8(buffer)
            .map_err(|_| AttentionError::new("unsafe_tty", "tty path is not UTF-8")),
    )
}

/// Lists a mux's panes through `wezterm cli list`. It never starts a server.
#[derive(Clone, Copy, Debug, Default)]
pub struct WeztermPaneLister;

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessProbe;

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn named_wezterm(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new("wezterm"))
}

/// The id of the named group, when this system has one.
fn group_id(name: &str) -> Option<libc::gid_t> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut size = 4096;
    while size <= 1 << 20 {
        let mut group: libc::group = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0_u8; size];
        let mut found: *mut libc::group = std::ptr::null_mut();
        let status = unsafe {
            libc::getgrnam_r(
                name.as_ptr(),
                &mut group,
                buffer.as_mut_ptr().cast::<libc::c_char>(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE {
            size *= 4;
            continue;
        }
        return (status == 0 && !found.is_null()).then_some(group.gr_gid);
    }
    None
}

/// One directory or file on the way from a candidate up to `/`.
#[derive(Clone, Copy, Debug)]
struct Ancestor {
    owner: libc::uid_t,
    group: libc::gid_t,
    mode: u32,
    is_dir: bool,
}

/// Whether a candidate that did not come from PATH may be run.
///
/// Every directory from the file up to `/` must be owned by root or by this
/// user and writable by nobody else, so no other account could have put the
/// file there. The one relaxation is a root-owned directory whose group is
/// `admin` or `wheel`: macOS ships `/Applications` as root:admin 0775, and an
/// administrator can already replace anything on the machine.
fn is_trusted_fallback(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !named_wezterm(path) {
        return false;
    }
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    if !named_wezterm(&canonical) || !is_executable(&canonical) {
        return false;
    }
    let mut ancestry = Vec::new();
    for component in canonical.ancestors() {
        let Ok(metadata) = fs::metadata(component) else {
            return false;
        };
        ancestry.push(Ancestor {
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.permissions().mode(),
            is_dir: metadata.is_dir(),
        });
    }
    let administrators = [group_id("admin"), group_id("wheel")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    ancestry_is_trusted(&ancestry, unsafe { libc::geteuid() }, &administrators)
}

fn ancestry_is_trusted(
    ancestry: &[Ancestor],
    current_uid: libc::uid_t,
    administrators: &[libc::gid_t],
) -> bool {
    ancestry.iter().all(|ancestor| {
        let owned = ancestor.owner == 0 || ancestor.owner == current_uid;
        let administered =
            ancestor.owner == 0 && ancestor.is_dir && administrators.contains(&ancestor.group);
        owned && ancestor.mode & 0o002 == 0 && (ancestor.mode & 0o020 == 0 || administered)
    })
}

/// Find the `wezterm` CLI.
///
/// Candidates, in order: `wezterm` in each absolute PATH entry; `wezterm` in
/// `executable_dir` (WezTerm's `WEZTERM_EXECUTABLE_DIR`); `wezterm` beside
/// `executable` (WezTerm's `WEZTERM_EXECUTABLE`); then `fallbacks`. Every
/// candidate after PATH must pass the ownership check above.
///
/// Only a file named exactly `wezterm` is ever returned. WezTerm sets
/// `WEZTERM_EXECUTABLE` in each pane to the GUI or the mux server, never to
/// the CLI; the GUI rejects `cli ... list`, and the mux server runs it as a
/// program after binding the default socket in place of a live server's, so
/// that variable only says which directory to look in. A relative PATH entry
/// names whatever directory the caller happens to be in, so it is skipped.
pub fn resolve_wezterm_executable(
    path_value: Option<&OsStr>,
    executable_dir: Option<&OsStr>,
    executable: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> Result<PathBuf> {
    if let Some(paths) = path_value {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join("wezterm");
            if directory.is_absolute() && is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    let beside_running = [
        executable_dir.map(PathBuf::from),
        executable
            .map(Path::new)
            .and_then(Path::parent)
            .map(Path::to_path_buf),
    ];
    let installed = beside_running
        .into_iter()
        .flatten()
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("wezterm"))
        .chain(fallbacks.iter().cloned());
    for candidate in installed {
        if is_trusted_fallback(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AttentionError::new(
        "realm_unavailable",
        "wezterm executable was not found",
    ))
}

pub fn wezterm_executable() -> Result<PathBuf> {
    resolve_wezterm_executable(
        env::var_os("PATH").as_deref(),
        env::var_os("WEZTERM_EXECUTABLE_DIR").as_deref(),
        env::var_os("WEZTERM_EXECUTABLE").as_deref(),
        &[PathBuf::from(
            "/Applications/WezTerm.app/Contents/MacOS/wezterm",
        )],
    )
}

pub fn parse_pane_rows(bytes: &[u8]) -> Result<Vec<PaneRow>> {
    if bytes.len() > manifest()?.limits.max_json_bytes {
        return Err(AttentionError::new(
            "record_invalid",
            "wezterm cli list exceeded its JSON bound",
        ));
    }
    serde_json::from_slice::<Vec<PaneRow>>(bytes).map_err(|_| {
        AttentionError::new("record_invalid", "wezterm cli list returned malformed rows")
    })
}

impl PaneLister for WeztermPaneLister {
    fn list(&self, socket_path: &str) -> Result<Vec<PaneRow>> {
        parse_pane_rows(&list_wezterm_inventory(socket_path)?)
    }
}

/// Whether the socket file at `socket_path` refuses a connection, which says
/// nothing listens on that file: its server has exited. WezTerm judges a GUI
/// socket dead by the same test. A listener that exists but is slow, busy or
/// unreadable to this user does not refuse, and neither does a path this
/// check cannot name; only `ECONNREFUSED` counts. The connect does not block,
/// so a listener with a full backlog cannot stall a read.
pub(crate) fn listener_refuses(socket_path: &str) -> bool {
    let bytes = socket_path.as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return false;
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return false;
    }
    // Closed on every return.
    let _socket = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let prepared = unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0
            && libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            ) == 0
    };
    if !prepared {
        return false;
    }
    let connected = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    connected != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECONNREFUSED)
}

/// Whether `socket_path` names a WezTerm GUI's own socket, `gui-sock-<pid>`,
/// and that process has exited. A GUI's local panes end with the GUI, so
/// none of them can still run. A pid that answers, or one this user may not
/// signal, may be the GUI, or a process that took its number: neither shows
/// the GUI gone.
pub(crate) fn gui_process_exited(socket_path: &str) -> bool {
    let Some(pid) = Path::new(socket_path)
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix("gui-sock-"))
        .filter(|pid| {
            !pid.is_empty() && !pid.starts_with('0') && pid.bytes().all(|b| b.is_ascii_digit())
        })
        .and_then(|pid| pid.parse::<libc::pid_t>().ok())
    else {
        return false;
    };
    let signalled = unsafe { libc::kill(pid, 0) };
    signalled != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// How long any one child this crate runs may take, output included.
const CHILD_DEADLINE: Duration = Duration::from_secs(5);

fn list_wezterm_inventory(socket_path: &str) -> Result<Vec<u8>> {
    let executable = wezterm_executable()?;
    let maximum = manifest()?.limits.max_json_bytes;
    let mut command = Command::new(&executable);
    // `--no-auto-start` on every call: without it a leftover socket with no
    // server behind it makes the CLI retry for seconds and then start a mux
    // server, so reading state would create a mux.
    command
        .args([
            "--skip-config",
            "cli",
            "--prefer-mux",
            "--no-auto-start",
            "list",
            "--format",
            "json",
        ])
        .env_clear()
        .env("WEZTERM_UNIX_SOCKET", socket_path);
    run_bounded(&mut command, maximum, CHILD_DEADLINE).map_err(|failure| {
        let code = match failure {
            RunFailure::TooLarge => "record_invalid",
            _ => "realm_unavailable",
        };
        AttentionError::new(
            code,
            format!(
                "wezterm cli list via {} {}",
                shown_path(&executable),
                failure.describe()
            ),
        )
    })
}

/// A path as a diagnostic prints it: control characters become `?`, so a
/// path cannot restyle the terminal that reads the message.
fn shown_path(path: &Path) -> String {
    path.display()
        .to_string()
        .chars()
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Why a bounded child run returned no output.
#[derive(Clone, Copy, Debug)]
enum RunFailure {
    NotStarted,
    TimedOut(Duration),
    Exited(std::process::ExitStatus),
    Unreadable,
    TooLarge,
}

impl RunFailure {
    fn describe(self) -> String {
        use std::os::unix::process::ExitStatusExt;
        match self {
            Self::NotStarted => "could not be started".to_owned(),
            Self::TimedOut(limit) => format!("timed out after {} ms", limit.as_millis()),
            Self::Exited(status) => match (status.code(), status.signal()) {
                (Some(code), _) => format!("exited with status {code}"),
                (None, Some(signal)) => format!("was killed by signal {signal}"),
                (None, None) => "exited abnormally".to_owned(),
            },
            Self::Unreadable => "output could not be read".to_owned(),
            Self::TooLarge => "output exceeded its bound".to_owned(),
        }
    }
}

/// Run `command` with no stdin and no stderr, and return its stdout if it
/// exits successfully within `limit` having written at most `maximum` bytes.
///
/// The child leads its own process group, and the whole group is killed when
/// the deadline passes. Killing the child alone is not enough: a descendant
/// that inherited stdout keeps the pipe open after the child is gone, and a
/// reader waiting for end of file would wait as long as that descendant
/// lives. The reader is never joined for the same reason; it only reports
/// back through a channel.
fn run_bounded(
    command: &mut Command,
    maximum: usize,
    limit: Duration,
) -> std::result::Result<Vec<u8>, RunFailure> {
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc::{TryRecvError, channel};
    let deadline = Instant::now() + limit;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| RunFailure::NotStarted)?;
    let Some(stdout) = child.stdout.take() else {
        kill_group(&mut child);
        return Err(RunFailure::Unreadable);
    };
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(read);
    });
    let mut output = None;
    let mut status = None;
    loop {
        if output.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() > maximum => {
                    kill_group(&mut child);
                    return Err(RunFailure::TooLarge);
                }
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    kill_group(&mut child);
                    return Err(RunFailure::Unreadable);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(exited) => status = exited,
                Err(_) => {
                    kill_group(&mut child);
                    return Err(RunFailure::Unreadable);
                }
            }
        }
        match status {
            Some(exited) if !exited.success() => {
                kill_group(&mut child);
                return Err(RunFailure::Exited(exited));
            }
            Some(_) if output.is_some() => return output.ok_or(RunFailure::Unreadable),
            _ => {}
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            return Err(RunFailure::TimedOut(limit));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Kill every process in the group `child` leads, then reap `child`.
///
/// The group id is the child's pid, and it stays reserved while any member
/// lives, so this reaches a descendant even after the child was reaped.
fn kill_group(child: &mut std::process::Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl ProcessProbe for SystemProcessProbe {
    /// The same listing a presence question takes, so a probe reported
    /// healthy is one whose answers can be read.
    fn available(&self) -> bool {
        !matches!(self.pane_processes(), ProcessListing::Failed)
    }

    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        match self.pane_processes() {
            ProcessListing::Listed(processes) => processes.presence(socket_path, pane_id),
            ProcessListing::Failed | ProcessListing::NotOffered => Presence::Unavailable,
        }
    }

    fn pane_processes(&self) -> ProcessListing {
        match own_pane_processes() {
            Some(processes) => ProcessListing::Listed(processes),
            None => ProcessListing::Failed,
        }
    }
}

/// The pairs in the environments of this user's processes, read from the
/// kernel rather than from `ps`.
///
/// `ps eww` prints each process's arguments and environment as one line, so
/// `WEZTERM_PANE=` text in any process's arguments -- any user's -- read as
/// an environment, and on procps-ng the BSD flags it needs fail outright.
/// The kernel hands over the environment block alone. Reading this process's
/// own block is required: a listing that could not read even that one would
/// report every pane absent for want of permission, not for want of a pane.
#[cfg(target_os = "macos")]
fn own_pane_processes() -> Option<PaneProcessSet> {
    /// `PROC_UID_ONLY` from `<libproc.h>`: processes whose effective uid is
    /// the one given.
    const PROC_UID_ONLY: u32 = 4;
    let uid = unsafe { libc::geteuid() };
    let pid_size = std::mem::size_of::<libc::pid_t>();
    let needed = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    let needed = usize::try_from(needed).ok().filter(|bytes| *bytes > 0)?;
    // Room for processes started between the two calls.
    let mut pids = vec![0 as libc::pid_t; needed / pid_size + 256];
    let capacity = libc::c_int::try_from(pids.len() * pid_size).ok()?;
    let filled =
        unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), capacity) };
    pids.truncate(usize::try_from(filled).ok()? / pid_size);
    let mut argument_bytes: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut name = [libc::CTL_KERN, libc::KERN_ARGMAX];
    if unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            2,
            (&mut argument_bytes as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    let mut buffer = vec![0_u8; usize::try_from(argument_bytes).ok()?];
    let own = libc::pid_t::try_from(std::process::id()).ok()?;
    let mut read_own = false;
    let mut processes = PaneProcessSet::default();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size = buffer.len();
        // A process that exited is not listed. One the kernel will not
        // describe is still running, and whatever it carries is missed.
        if unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            if still_running(pid) {
                processes.missed_one();
            }
            continue;
        }
        read_own |= pid == own;
        let environment =
            procargs_environment(&buffer[..size.min(buffer.len())]).collect::<Vec<_>>();
        // macOS hands over the arguments of its own system binaries, /bin/zsh
        // and /bin/sleep among them, with no environment at all. A process
        // started with an empty environment looks the same, and is counted
        // the same way: nothing can be read from it.
        if environment.is_empty() {
            processes.missed_one();
        }
        processes.add_environment(environment.into_iter());
    }
    read_own.then_some(processes)
}

/// Whether `pid` names a process that has not exited, whoever may signal it.
#[cfg(target_os = "macos")]
fn still_running(pid: libc::pid_t) -> bool {
    let signalled = unsafe { libc::kill(pid, 0) };
    signalled == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "linux")]
fn own_pane_processes() -> Option<PaneProcessSet> {
    use std::os::unix::fs::MetadataExt;
    let uid = unsafe { libc::geteuid() };
    let own = std::process::id().to_string();
    let mut read_own = false;
    let mut processes = PaneProcessSet::default();
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|name| name.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        // `/proc/<pid>` belongs to the process's effective uid.
        if entry.metadata().ok().map(|metadata| metadata.uid()) != Some(uid) {
            continue;
        }
        let environment = match fs::read(entry.path().join("environ")) {
            Ok(environment) => environment,
            // The process exited after it was listed.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            // Still running, and not readable by this user: a process that
            // is not dumpable, for one.
            Err(_) => {
                processes.missed_one();
                continue;
            }
        };
        read_own |= pid == own;
        processes.add_environment(environment.split(|byte| *byte == 0));
    }
    read_own.then_some(processes)
}

/// Elsewhere only `ps` offers environments, and it prints them after the
/// arguments on one line, so lines are kept only for this user's processes.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn own_pane_processes() -> Option<PaneProcessSet> {
    let ps = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .map(Path::new)
        .find(|path| is_executable(path))?;
    let mut command = Command::new(ps);
    command.args(["axeww", "-o", "uid=,command="]);
    let output = run_bounded(&mut command, 8 * 1024 * 1024, CHILD_DEADLINE).ok()?;
    let uid = unsafe { libc::geteuid() }.to_string();
    let listing = String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| {
            let (owner, rest) = line.trim_start().split_once(' ')?;
            (owner == uid).then_some(rest)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(PaneProcessSet::from_process_listing(&listing))
}

/// The environment strings in a `KERN_PROCARGS2` buffer.
///
/// The buffer holds `argc`, the executable path, NUL padding, `argc`
/// argument strings, then the environment strings, ended by an empty string
/// or by the end of the buffer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn procargs_environment(buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    let count = buffer
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(i32::from_ne_bytes)
        .and_then(|count| usize::try_from(count).ok());
    let mut strings = buffer.get(4..).unwrap_or_default().split(|byte| *byte == 0);
    let arguments_known = count.is_some() && strings.next().is_some();
    let mut rest = strings.skip_while(|string| string.is_empty());
    for _ in 0..count.unwrap_or(0) {
        rest.next();
    }
    rest.take_while(move |string| arguments_known && !string.is_empty())
}

/// The values `name` takes on one process line, where `name` ends in `=`.
///
/// A value may contain spaces -- a socket path can -- so it runs to the next
/// ` NAME=` that starts another variable, or to the end of the line.
fn environment_values<'a>(line: &'a str, name: &str) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(relative) = line[offset..].find(name) {
        let start = offset + relative;
        let value_start = start + name.len();
        if start == 0 || line.as_bytes()[start - 1] == b' ' {
            let rest = &line[value_start..];
            values.push(&rest[..next_variable_boundary(rest)]);
        }
        offset = value_start;
    }
    values
}

fn next_variable_boundary(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    while let Some(relative) = text[index..].find(' ') {
        let space = index + relative;
        let name = &bytes[space + 1..];
        let length = name
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
            .count();
        if length > 0 && !name[0].is_ascii_digit() && name.get(length) == Some(&b'=') {
            return space;
        }
        index = space + 1;
    }
    text.len()
}

pub fn default_ports<'a>(
    clock: &'a SystemClock,
    tty: &'a SystemTtyWriter,
    panes: &'a WeztermPaneLister,
) -> RuntimePorts<'a> {
    RuntimePorts { clock, tty, panes }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn osc(name: &str, value: &str) -> Vec<u8> {
    format!(
        "\x1b]1337;SetUserVar={name}={}\x07",
        base64(value.as_bytes())
    )
    .into_bytes()
}

pub fn publication_bytes(address: &PaneAddress, launch_id: Option<&str>) -> Result<Vec<u8>> {
    let mut bytes = osc("WEZTERM_PANE", &address.pane_id);
    if let Some(launch_id) = launch_id {
        let wire = serde_json::json!({
            "wire": manifest()?.wire_version,
            "address": address,
            "launch_id": launch_id,
        });
        if parse_wire_value(&wire, manifest()?) != Verdict::Valid {
            return Err(AttentionError::new(
                "record_invalid",
                "outgoing publication does not match the wire manifest",
            ));
        }
        let encoded = serde_json::to_string(&wire).map_err(AttentionError::record_json)?;
        bytes.extend(osc("WEZTERM_ATTENTION", &encoded));
    }
    if bytes.len() > 8192 {
        return Err(AttentionError::new(
            "record_invalid",
            "OSC publication exceeds its 8192-byte bound",
        ));
    }
    Ok(bytes)
}

pub fn file_from_fd(fd: libc::c_int) -> File {
    unsafe { File::from_raw_fd(fd) }
}

pub fn tty_path_from_fd(fd: libc::c_int) -> Result<String> {
    let file = file_from_fd(fd);
    let name = ttyname(file.as_raw_fd());
    std::mem::forget(file);
    name.ok_or_else(|| AttentionError::new("unsafe_tty", "descriptor is not a tty"))?
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use super::{
        Ancestor, PaneProcessSet, Presence, SystemTtyWriter, ancestry_is_trusted, tty_path_from_fd,
    };
    use crate::identity::tty_fingerprint;

    const USER: libc::uid_t = 501;
    const ADMIN: libc::gid_t = 80;
    const STAFF: libc::gid_t = 20;

    fn directory(owner: libc::uid_t, group: libc::gid_t, mode: u32) -> Ancestor {
        Ancestor {
            owner,
            group,
            mode: 0o040000 | mode,
            is_dir: true,
        }
    }

    fn file(owner: libc::uid_t, group: libc::gid_t, mode: u32) -> Ancestor {
        Ancestor {
            owner,
            group,
            mode: 0o100000 | mode,
            is_dir: false,
        }
    }

    #[test]
    fn an_application_below_a_root_admin_group_writable_directory_is_trusted() {
        // /Applications/WezTerm.app/Contents/MacOS/wezterm on a stock Mac.
        let ancestry = [
            file(USER, ADMIN, 0o755),
            directory(USER, ADMIN, 0o755),
            directory(USER, ADMIN, 0o755),
            directory(USER, ADMIN, 0o755),
            directory(0, ADMIN, 0o775),
            directory(0, 0, 0o755),
        ];
        assert!(ancestry_is_trusted(&ancestry, USER, &[ADMIN, 0]));
    }

    #[test]
    fn group_write_is_trusted_only_on_a_root_directory_of_an_administrator_group() {
        let root = directory(0, 0, 0o755);
        for (label, ancestor) in [
            ("root directory, ordinary group", directory(0, STAFF, 0o775)),
            ("user directory, admin group", directory(USER, ADMIN, 0o775)),
            ("root file, admin group", file(0, ADMIN, 0o775)),
            (
                "root admin directory writable by other",
                directory(0, ADMIN, 0o777),
            ),
            ("sticky world-writable directory", directory(0, 0, 0o1777)),
            ("another account's directory", directory(502, STAFF, 0o755)),
        ] {
            assert!(
                !ancestry_is_trusted(&[ancestor, root], USER, &[ADMIN, 0]),
                "{label} must not be trusted"
            );
        }
        assert!(ancestry_is_trusted(
            &[directory(USER, STAFF, 0o700), root],
            USER,
            &[ADMIN, 0]
        ));
    }

    /// A terminal that takes `accepted` bytes, then takes nothing until
    /// `stalled_until`, then takes everything.
    struct StallingTerminal {
        accepted: usize,
        stalled_until: std::time::Instant,
        received: Vec<u8>,
    }

    impl std::io::Write for StallingTerminal {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let room = if std::time::Instant::now() >= self.stalled_until {
                data.len()
            } else {
                self.accepted.saturating_sub(self.received.len())
            };
            if room == 0 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let count = room.min(data.len());
            self.received.extend_from_slice(&data[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_cut_short_by_a_stall_closes_its_sequence_once_the_stall_clears() {
        let data = super::osc("WEZTERM_PANE", "1234");
        let started = std::time::Instant::now();
        let mut terminal = StallingTerminal {
            accepted: 34,
            stalled_until: started
                + super::TTY_WRITE_TIMEOUT
                + std::time::Duration::from_millis(100),
            received: Vec::new(),
        };
        let result = super::write_tty_with_deadline(&mut terminal, &data);
        assert!(result.is_err(), "the write stalled past its deadline");
        assert_eq!(terminal.received, [&data[..34], b"!\x07"].concat());
        assert!(
            started.elapsed()
                < super::TTY_WRITE_TIMEOUT * 2 + std::time::Duration::from_millis(100)
        );

        let mut stalled = StallingTerminal {
            accepted: 34,
            stalled_until: started + std::time::Duration::from_secs(60),
            received: Vec::new(),
        };
        assert!(super::write_tty_with_deadline(&mut stalled, &data).is_err());
        assert_eq!(
            stalled.received,
            &data[..34],
            "nothing more lands on a tty still stalled"
        );
    }

    #[test]
    fn a_cut_publication_is_closed_so_that_it_cannot_apply() {
        let data = [
            super::osc("WEZTERM_PANE", "1234"),
            super::osc("WEZTERM_ATTENTION", "{}"),
        ]
        .concat();
        let first_end = data.iter().position(|byte| *byte == 0x07).unwrap() + 1;
        // Cut inside the first value, at a base64 boundary ("MTIz" is "123").
        let value_start = data
            .windows(4)
            .position(|window| window == b"MTIz")
            .unwrap();
        assert_eq!(
            super::cut_sequence_closing(&data, value_start + 4),
            b"!\x07"
        );
        assert_eq!(super::cut_sequence_closing(&data, 1), b"\\");
        assert_eq!(super::cut_sequence_closing(&data, first_end), b"");
        assert_eq!(super::cut_sequence_closing(&data, first_end + 1), b"\\");
        assert_eq!(super::cut_sequence_closing(&data, first_end + 5), b"!\x07");
        assert_eq!(super::cut_sequence_closing(&data, 0), b"");
        assert_eq!(super::cut_sequence_closing(&data, data.len()), b"");
    }

    #[test]
    fn only_the_environment_part_of_a_process_argument_block_is_read() {
        let mut buffer = 2_i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(
            b"/bin/sh\0\0\0\0sh\0WEZTERM_PANE=9 WEZTERM_UNIX_SOCKET=/argv.sock\0\
              HOME=/h\0WEZTERM_UNIX_SOCKET=/env.sock\0WEZTERM_PANE=7\0\0executable_path=/bin/sh\0",
        );
        let environment = super::procargs_environment(&buffer).collect::<Vec<_>>();
        assert_eq!(
            environment,
            [
                &b"HOME=/h"[..],
                b"WEZTERM_UNIX_SOCKET=/env.sock",
                b"WEZTERM_PANE=7"
            ]
        );
        let mut processes = PaneProcessSet::default();
        processes.add_environment(environment.into_iter());
        assert_eq!(processes.presence("/env.sock", "7"), Presence::Present);
        assert_eq!(processes.presence("/argv.sock", "9"), Presence::Absent);
        assert_eq!(super::procargs_environment(&[1, 0]).count(), 0);
    }

    #[test]
    fn a_socket_path_with_an_interior_space_is_read_whole() {
        // In the file name, so the directory is one that resolves and a
        // value cut at the space compares as a different socket.
        let processes = PaneProcessSet::from_process_listing(
            "cmd WEZTERM_UNIX_SOCKET=/domain with space.sock WEZTERM_PANE=42 other=1",
        );
        assert_eq!(
            processes.presence("/domain with space.sock", "42"),
            Presence::Present
        );
        assert_eq!(
            processes.presence("/domain with space.sock", "4"),
            Presence::Absent
        );
        assert_eq!(processes.presence("/domain", "42"), Presence::Absent);
    }

    #[test]
    fn a_pane_is_present_only_on_the_socket_its_own_process_names() {
        let processes = PaneProcessSet::from_process_listing(
            "zsh WEZTERM_PANE=7 WEZTERM_UNIX_SOCKET=/a.sock\nzsh WEZTERM_UNIX_SOCKET=/b.sock WEZTERM_PANE=8\n",
        );
        assert_eq!(processes.presence("/a.sock", "7"), Presence::Present);
        assert_eq!(processes.presence("/b.sock", "8"), Presence::Present);
        assert_eq!(processes.presence("/a.sock", "8"), Presence::Absent);
        assert_eq!(processes.presence("/b.sock", "7"), Presence::Absent);
    }

    #[test]
    fn a_listing_that_missed_a_process_never_shows_a_pane_absent() {
        let mut processes =
            PaneProcessSet::from_process_listing("zsh WEZTERM_PANE=7 WEZTERM_UNIX_SOCKET=/a.sock");
        assert_eq!(processes.presence("/a.sock", "8"), Presence::Absent);
        processes.missed_one();
        assert_eq!(processes.presence("/a.sock", "7"), Presence::Present);
        assert_eq!(processes.presence("/a.sock", "8"), Presence::Unseen);
    }

    #[test]
    fn a_socket_is_compared_by_its_resolved_directory_and_file_name() {
        let base = std::env::temp_dir().join(format!("wa-resolve-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).expect("socket directory");
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let resolved = std::fs::canonicalize(&real)
            .expect("resolve")
            .join("gone.sock");
        let processes = PaneProcessSet::from_process_listing(&format!(
            "agent WEZTERM_PANE=7 WEZTERM_UNIX_SOCKET={}",
            link.join("gone.sock").display()
        ));
        assert_eq!(
            processes.presence(resolved.to_str().expect("UTF-8"), "7"),
            Presence::Present,
            "the socket file need not exist"
        );
        assert_eq!(
            processes.presence(
                resolved
                    .with_file_name("other.sock")
                    .to_str()
                    .expect("UTF-8"),
                "7"
            ),
            Presence::Absent
        );
        // A directory that cannot be resolved cannot be compared, so a
        // process with the same pane id there might be the pane's.
        let unresolved = PaneProcessSet::from_process_listing(
            "agent WEZTERM_PANE=7 WEZTERM_UNIX_SOCKET=/does-not-exist-synthetic/mux.sock",
        );
        assert_eq!(
            unresolved.presence(resolved.to_str().expect("UTF-8"), "7"),
            Presence::Unseen
        );
        assert_eq!(
            unresolved.presence(resolved.to_str().expect("UTF-8"), "8"),
            Presence::Absent
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_name_that_only_ends_like_the_variable_is_not_the_variable() {
        let processes = PaneProcessSet::from_process_listing(
            "zsh NOT_WEZTERM_PANE=7 WEZTERM_UNIX_SOCKET=/a.sock",
        );
        assert_eq!(processes.presence("/a.sock", "7"), Presence::Absent);
    }

    #[test]
    fn nothing_else_in_a_process_environment_is_kept() {
        let processes = PaneProcessSet::from_process_listing(
            "zsh API_KEY=hunter2 WEZTERM_UNIX_SOCKET=/a.sock TOKEN=swordfish WEZTERM_PANE=7 LAST=opensesame",
        );
        assert_eq!(processes.presence("/a.sock", "7"), Presence::Present);
        let kept = format!("{processes:?}");
        for leaked in [
            "API_KEY",
            "hunter2",
            "TOKEN",
            "swordfish",
            "LAST",
            "opensesame",
        ] {
            assert!(!kept.contains(leaked), "{leaked} survived parsing: {kept}");
        }
    }

    #[test]
    fn write_revalidates_the_descriptor_returned_by_open() {
        let mut first_master = 0;
        let mut first_slave = 0;
        let mut second_master = 0;
        let mut second_slave = 0;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut first_master,
                    &mut first_slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut second_master,
                    &mut second_slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let first_path = tty_path_from_fd(first_slave).expect("first tty path");
        let second_path = tty_path_from_fd(second_slave).expect("second tty path");
        let expected = tty_fingerprint(&first_path).expect("first fingerprint");
        let error = SystemTtyWriter
            .write_with_open(&first_path, b"must-not-write", &expected, |_| {
                OpenOptions::new().write(true).open(&second_path)
            })
            .expect_err("write must reject a substituted descriptor");
        assert_eq!(error.diagnostic.code, "unsafe_tty");
        unsafe {
            libc::close(first_master);
            libc::close(first_slave);
            libc::close(second_master);
            libc::close(second_slave);
        }
    }
}
