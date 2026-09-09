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

pub trait ProcessProbe: Send + Sync {
    fn available(&self) -> bool;
    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    Present,
    Absent,
    Unavailable,
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

fn write_tty_with_deadline(file: &mut File, data: &[u8]) -> std::io::Result<()> {
    let deadline = Instant::now() + TTY_WRITE_TIMEOUT;
    let mut written = 0;
    while written < data.len() {
        match file.write(&data[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "tty write made no progress",
                ));
            }
            Ok(count) => written += count,
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
    let mut buffer = vec![0_i8; 4096];
    let result = unsafe { libc::ttyname_r(fd, buffer.as_mut_ptr(), buffer.len()) };
    if result != 0 {
        return Err(AttentionError::new("unsafe_tty", "stdin is not a terminal"));
    }
    let bytes = buffer
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8(bytes).map_err(|_| AttentionError::new("unsafe_tty", "tty path is not UTF-8"))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WeztermPaneLister;

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessProbe;

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn is_trusted_fallback(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    if !is_executable(&canonical) {
        return false;
    }
    let current_uid = unsafe { libc::geteuid() };
    for component in canonical.ancestors() {
        let Ok(metadata) = fs::metadata(component) else {
            return false;
        };
        if !matches!(metadata.uid(), 0) && metadata.uid() != current_uid {
            return false;
        }
        if metadata.permissions().mode() & 0o022 != 0 {
            return false;
        }
    }
    true
}

pub fn resolve_wezterm_executable(
    path_value: Option<&OsStr>,
    configured: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> Result<PathBuf> {
    if let Some(paths) = path_value {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join("wezterm");
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    if let Some(configured) = configured {
        let candidate = PathBuf::from(configured);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    for candidate in fallbacks {
        if is_trusted_fallback(candidate) {
            return Ok(candidate.clone());
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
        let executable = wezterm_executable()?;
        let mut child = Command::new(executable)
            .args([
                "--skip-config",
                "cli",
                "--prefer-mux",
                "list",
                "--format",
                "json",
            ])
            .env_clear()
            .env("WEZTERM_UNIX_SOCKET", socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| AttentionError::new("realm_unavailable", "wezterm cli list failed"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AttentionError::new(
                "realm_unavailable",
                "wezterm cli list stdout is unavailable",
            )
        })?;
        let maximum = manifest()?.limits.max_json_bytes;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take((maximum + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(AttentionError::new(
                        "realm_unavailable",
                        "wezterm cli list timed out",
                    ));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(AttentionError::new(
                        "realm_unavailable",
                        "wezterm cli list failed",
                    ));
                }
            }
        };
        let bytes = reader
            .join()
            .map_err(|_| {
                AttentionError::new("realm_unavailable", "wezterm cli list reader failed")
            })?
            .map_err(|_| {
                AttentionError::new("realm_unavailable", "wezterm cli list could not be read")
            })?;
        if !status.success() {
            return Err(AttentionError::new(
                "realm_unavailable",
                "wezterm cli list failed",
            ));
        }
        parse_pane_rows(&bytes)
    }
}

impl ProcessProbe for SystemProcessProbe {
    fn available(&self) -> bool {
        let child = Command::new("/bin/ps")
            .args(["-axo", "pid="])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    fn presence(&self, socket_path: &str, pane_id: &str) -> Presence {
        let child = Command::new("/bin/ps")
            .args(["eww", "-axo", "command="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return Presence::Unavailable;
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Presence::Unavailable;
        };
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let output = reader.join().ok().and_then(std::result::Result::ok);
        let (Some(status), Some(output)) = (status, output) else {
            return Presence::Unavailable;
        };
        if !status.success() || output.len() > 8 * 1024 * 1024 {
            return Presence::Unavailable;
        }
        let socket_token = format!("WEZTERM_UNIX_SOCKET={socket_path}");
        let pane_token = format!("WEZTERM_PANE={pane_id}");
        let present = String::from_utf8_lossy(&output).lines().any(|line| {
            has_environment_token(line, &socket_token) && has_environment_token(line, &pane_token)
        });
        if present {
            Presence::Present
        } else {
            Presence::Absent
        }
    }
}

fn has_environment_token(line: &str, token: &str) -> bool {
    let mut offset = 0;
    while let Some(relative) = line[offset..].find(token) {
        let start = offset + relative;
        let end = start + token.len();
        let before_ok = start == 0 || line.as_bytes()[start - 1] == b' ';
        let after_ok = end == line.len() || line.as_bytes()[end] == b' ';
        if before_ok && after_ok {
            return true;
        }
        offset = start + 1;
    }
    false
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
    let raw = file.as_raw_fd();
    let mut buffer = vec![0_i8; 4096];
    let result = unsafe { libc::ttyname_r(raw, buffer.as_mut_ptr(), buffer.len()) };
    std::mem::forget(file);
    if result != 0 {
        return Err(AttentionError::new("unsafe_tty", "descriptor is not a tty"));
    }
    let bytes = buffer
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8(bytes).map_err(|_| AttentionError::new("unsafe_tty", "tty path is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use super::{SystemTtyWriter, has_environment_token, tty_path_from_fd};
    use crate::identity::tty_fingerprint;

    #[test]
    fn process_environment_token_accepts_an_interior_space() {
        let line =
            "cmd WEZTERM_UNIX_SOCKET=/tmp/domain with space/mux.sock WEZTERM_PANE=42 other=1";
        assert!(has_environment_token(
            line,
            "WEZTERM_UNIX_SOCKET=/tmp/domain with space/mux.sock"
        ));
        assert!(has_environment_token(line, "WEZTERM_PANE=42"));
        assert!(!has_environment_token(line, "WEZTERM_PANE=4"));
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
