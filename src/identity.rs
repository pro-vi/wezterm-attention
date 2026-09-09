use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::protocol::{AttentionError, Result, manifest};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneAddress {
    pub realm_id: String,
    pub incarnation_id: String,
    pub pane_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketMetadata {
    pub socket_path: String,
    pub socket_device: String,
    pub socket_inode: String,
    pub socket_ctime_ns: String,
}

pub fn canonical_pane_id(value: &str) -> Result<String> {
    let maximum = manifest()?.limits.pane_id_max_digits;
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(AttentionError::new(
            "record_invalid",
            "WEZTERM_PANE is not canonical",
        ));
    }
    Ok(value.to_owned())
}

pub fn canonical_uuid(value: Option<&str>, name: &str) -> Result<String> {
    let Some(value) = value else {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{name} is not a UUID"),
        ));
    };
    let parsed = Uuid::parse_str(value)
        .map_err(|_| AttentionError::new("record_invalid", format!("{name} is not a UUID")))?;
    if parsed.to_string() != value {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{name} is not canonical"),
        ));
    }
    Ok(value.to_owned())
}

pub fn length_prefixed_digest<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        let bytes = part.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

pub fn socket_identity(socket_value: &str) -> Result<(String, String, SocketMetadata)> {
    if !Path::new(socket_value).is_absolute() {
        return Err(AttentionError::new(
            "record_invalid",
            "WEZTERM_UNIX_SOCKET must be absolute",
        ));
    }
    let canonical = fs::canonicalize(socket_value).map_err(|_| {
        AttentionError::new("probe_unavailable", "mux socket identity could not be read")
    })?;
    let metadata = fs::metadata(&canonical).map_err(|_| {
        AttentionError::new("probe_unavailable", "mux socket identity could not be read")
    })?;
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(AttentionError::new(
            "realm_unavailable",
            "mux socket is unavailable or unsafe",
        ));
    }
    let canonical = canonical
        .to_str()
        .ok_or_else(|| AttentionError::new("realm_unavailable", "mux socket path is not UTF-8"))?
        .to_owned();
    let realm_id = crate::protocol::sha256_hex(canonical.as_bytes());
    let device = metadata.dev().to_string();
    let inode = metadata.ino().to_string();
    let ctime_ns_value =
        i128::from(metadata.ctime()) * 1_000_000_000_i128 + i128::from(metadata.ctime_nsec());
    if ctime_ns_value < 0 {
        return Err(AttentionError::new(
            "clock_skew",
            "mux socket creation time is negative",
        ));
    }
    let ctime_ns = format!("{ctime_ns_value:020}");
    let incarnation_id = length_prefixed_digest([
        realm_id.as_str(),
        device.as_str(),
        inode.as_str(),
        ctime_ns_value.to_string().as_str(),
    ]);
    Ok((
        realm_id,
        incarnation_id,
        SocketMetadata {
            socket_path: canonical,
            socket_device: device,
            socket_inode: inode,
            socket_ctime_ns: ctime_ns,
        },
    ))
}

pub fn pane_address(env: &BTreeMap<String, String>) -> Result<(PaneAddress, SocketMetadata)> {
    let socket = env.get("WEZTERM_UNIX_SOCKET").ok_or_else(|| {
        AttentionError::new("identity_unpublished", "WEZTERM_UNIX_SOCKET is missing")
    })?;
    let (realm_id, incarnation_id, metadata) = socket_identity(socket)?;
    let pane_id = canonical_pane_id(env.get("WEZTERM_PANE").map(String::as_str).unwrap_or(""))?;
    Ok((
        PaneAddress {
            realm_id,
            incarnation_id,
            pane_id,
        },
        metadata,
    ))
}

pub fn tty_fingerprint_from_metadata(metadata: &fs::Metadata) -> Result<String> {
    if !metadata.file_type().is_char_device() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(AttentionError::new(
            "unsafe_tty",
            "tty is not a same-UID character device",
        ));
    }
    let device = metadata.dev().to_string();
    let inode = metadata.ino().to_string();
    let rdevice = metadata.rdev().to_string();
    Ok(length_prefixed_digest([
        device.as_str(),
        inode.as_str(),
        rdevice.as_str(),
    ]))
}

pub fn tty_fingerprint(path: &str) -> Result<String> {
    if !Path::new(path).is_absolute() {
        return Err(AttentionError::new(
            "unsafe_tty",
            "tty path must be absolute",
        ));
    }
    let metadata =
        fs::metadata(path).map_err(|_| AttentionError::new("unsafe_tty", "tty is unavailable"))?;
    tty_fingerprint_from_metadata(&metadata)
}

pub fn monotonic_ns20() -> Result<String> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut value) };
    if result != 0 || value.tv_sec < 0 || value.tv_nsec < 0 {
        return Err(AttentionError::new(
            "probe_unavailable",
            "monotonic clock is unavailable",
        ));
    }
    let nanoseconds = (value.tv_sec as u128) * 1_000_000_000_u128 + value.tv_nsec as u128;
    Ok(format!("{nanoseconds:020}"))
}
