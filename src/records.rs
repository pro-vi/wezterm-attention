use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::identity::PaneAddress;
use crate::protocol::{AttentionError, Result, manifest, validate_record};

#[derive(Clone, Debug, Default)]
pub struct RecordIdentity {
    address: Option<PaneAddress>,
    realm_id: Option<String>,
    incarnation_id: Option<String>,
    launch_id: Option<String>,
    binding_id: Option<String>,
    key: Option<(String, String)>,
    launch_target: bool,
}

impl RecordIdentity {
    pub fn unscoped() -> Self {
        Self::default()
    }

    pub fn realm(realm_id: &str) -> Self {
        Self {
            realm_id: Some(realm_id.to_owned()),
            ..Self::default()
        }
    }

    pub fn incarnation(realm_id: &str, incarnation_id: &str) -> Self {
        Self {
            realm_id: Some(realm_id.to_owned()),
            incarnation_id: Some(incarnation_id.to_owned()),
            ..Self::default()
        }
    }

    pub fn pane(address: &PaneAddress) -> Self {
        Self {
            address: Some(address.clone()),
            ..Self::default()
        }
    }

    pub fn launch(address: &PaneAddress, launch_id: &str) -> Self {
        Self {
            address: Some(address.clone()),
            launch_id: Some(launch_id.to_owned()),
            launch_target: true,
            ..Self::default()
        }
    }

    pub fn binding(address: &PaneAddress, launch_id: &str, binding_id: &str) -> Self {
        Self {
            address: Some(address.clone()),
            launch_id: Some(launch_id.to_owned()),
            binding_id: Some(binding_id.to_owned()),
            ..Self::default()
        }
    }

    pub fn review(address: &PaneAddress, owner_key: &str) -> Self {
        let mut identity = Self::pane(address);
        identity.key = Some(("owner_key".to_owned(), owner_key.to_owned()));
        identity
    }

    pub fn agent(
        address: &PaneAddress,
        launch_id: &str,
        binding_id: &str,
        agent_key: &str,
    ) -> Self {
        let mut identity = Self::binding(address, launch_id, binding_id);
        identity.key = Some(("agent_key".to_owned(), agent_key.to_owned()));
        identity
    }

    pub fn from_record(value: &Value) -> Self {
        let address = value
            .get("address")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let realm_id = value
            .get("realm_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let incarnation_id = value
            .get("incarnation_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let launch_id = value
            .get("launch_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let target = value.get("target").and_then(Value::as_object);
        let binding_id = value
            .get("binding_id")
            .and_then(Value::as_str)
            .or_else(|| {
                target
                    .and_then(|target| target.get("binding_id"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned);
        let key = ["owner_key", "agent_key"].into_iter().find_map(|field| {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(|value| (field.to_owned(), value.to_owned()))
        });
        Self {
            address,
            realm_id,
            incarnation_id,
            launch_id,
            binding_id,
            key,
            launch_target: target
                .is_some_and(|target| target.get("kind").and_then(Value::as_str) == Some("launch")),
        }
    }

    pub fn from_state_path(root: &Path, path: &Path, kind: &str) -> Result<Self> {
        let parts: Vec<_> = path
            .strip_prefix(root)
            .map_err(|_| AttentionError::new("record_invalid", "state path is outside its root"))?
            .iter()
            .map(|part| part.to_str())
            .collect();
        let invalid =
            || AttentionError::new("record_invalid", "state record path has the wrong shape");
        if parts.len() < 4 || parts[0] != Some("v2") || parts[1] != Some("realms") {
            return Err(invalid());
        }
        let realm = parts[2].ok_or_else(invalid)?;
        if kind == "realm" {
            return if parts.len() == 4 && parts[3] == Some("realm.json") {
                Ok(Self::realm(realm))
            } else {
                Err(invalid())
            };
        }
        if kind == "incarnation" {
            return if parts.len() == 6
                && parts[3] == Some("incarnations")
                && parts[5] == Some("incarnation.json")
            {
                Ok(Self::incarnation(realm, parts[4].ok_or_else(invalid)?))
            } else {
                Err(invalid())
            };
        }
        if parts.len() < 8 || parts[3] != Some("incarnations") || parts[5] != Some("panes") {
            return Err(invalid());
        }
        let address = PaneAddress {
            realm_id: realm.to_owned(),
            incarnation_id: parts[4].ok_or_else(invalid)?.to_owned(),
            pane_id: parts[6].ok_or_else(invalid)?.to_owned(),
        };
        match kind {
            "claim" if parts.len() == 8 && parts[7] == Some("claim.json") => {
                return Ok(Self::pane(&address));
            }
            "absence_probe" if parts.len() == 8 && parts[7] == Some("absence-probe.json") => {
                return Ok(Self::pane(&address));
            }
            "review"
                if parts.len() == 9
                    && parts[7] == Some("reviews")
                    && path.extension().and_then(|value| value.to_str()) == Some("json") =>
            {
                return Ok(Self::review(
                    &address,
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .ok_or_else(invalid)?,
                ));
            }
            _ => {}
        }
        if parts.len() < 10 || parts[7] != Some("launches") {
            return Err(invalid());
        }
        let launch_id = parts[8].ok_or_else(invalid)?;
        if kind == "current_binding"
            && parts.len() == 10
            && parts[9] == Some("current-binding.json")
        {
            return Ok(Self::launch(&address, launch_id));
        }
        if matches!(kind, "activity" | "acknowledgement")
            && parts.len() == 10
            && parts[9]
                == Some(if kind == "activity" {
                    "activity.json"
                } else {
                    "ack.json"
                })
        {
            return Ok(Self::launch(&address, launch_id));
        }
        if parts.len() < 12 || parts[9] != Some("bindings") {
            return Err(invalid());
        }
        let binding_id = parts[10].ok_or_else(invalid)?;
        if kind == "subagent_presence" {
            if parts.len() != 13
                || parts[11] != Some("agents")
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                return Err(invalid());
            }
            return Ok(Self::agent(
                &address,
                launch_id,
                binding_id,
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(invalid)?,
            ));
        }
        let expected_name = match kind {
            "binding" => "binding.json",
            "activity" => "activity.json",
            "activity_clear" => "activity-clear.json",
            "binding_end" => "end.json",
            "acknowledgement" => "ack.json",
            "subagent_clear" => "agents-clear.json",
            "subagent_retention_floor" => "agents-floor.json",
            _ => return Err(invalid()),
        };
        if parts.len() != 12 || parts[11] != Some(expected_name) {
            return Err(invalid());
        }
        Ok(Self::binding(&address, launch_id, binding_id))
    }

    pub fn validate(&self, record: &Value) -> Result<()> {
        if self.matches(record) {
            Ok(())
        } else {
            Err(AttentionError::new(
                "record_invalid",
                "state record identity does not match its path",
            ))
        }
    }

    fn matches(&self, record: &Value) -> bool {
        if let Some(expected) = &self.address
            && record.get("address") != serde_json::to_value(expected).ok().as_ref()
        {
            return false;
        }
        if let Some(expected) = &self.realm_id
            && record.get("realm_id").and_then(Value::as_str) != Some(expected)
        {
            return false;
        }
        if let Some(expected) = &self.incarnation_id
            && record.get("incarnation_id").and_then(Value::as_str) != Some(expected)
        {
            return false;
        }
        if let Some(expected) = &self.launch_id
            && record.get("launch_id").and_then(Value::as_str) != Some(expected)
        {
            return false;
        }
        if let Some(expected) = &self.binding_id {
            let actual = record
                .get("binding_id")
                .and_then(Value::as_str)
                .or_else(|| {
                    record
                        .get("target")
                        .and_then(Value::as_object)
                        .filter(|target| {
                            target.get("kind").and_then(Value::as_str) == Some("binding")
                        })
                        .and_then(|target| target.get("binding_id"))
                        .and_then(Value::as_str)
                });
            if actual != Some(expected) {
                return false;
            }
        } else if self.launch_target
            && record.get("target").is_some()
            && record
                .get("target")
                .and_then(Value::as_object)
                .and_then(|target| target.get("kind"))
                .and_then(Value::as_str)
                != Some("launch")
        {
            return false;
        }
        if let Some((field, expected)) = &self.key
            && record.get(field).and_then(Value::as_str) != Some(expected)
        {
            return false;
        }
        true
    }
}

#[derive(Clone, Debug)]
pub struct Replacement {
    pub path: PathBuf,
    pub value: Value,
    pub only_if_different: bool,
}

impl Replacement {
    pub fn always(path: PathBuf, value: Value) -> Self {
        Self {
            path,
            value,
            only_if_different: false,
        }
    }

    pub fn if_different(path: PathBuf, value: Value) -> Self {
        Self {
            path,
            value,
            only_if_different: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommitPlan<T> {
    pub result: T,
    pub replacements: Vec<Replacement>,
    pub removals: Vec<PathBuf>,
    pub private_dirs: Vec<PathBuf>,
}

pub fn state_root(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(path) = env.get("WEZTERM_ATTENTION_DIR") {
        return absolute_path(path, "WEZTERM_ATTENTION_DIR");
    }
    if let Some(path) = env.get("XDG_STATE_HOME") {
        return Ok(absolute_path(path, "XDG_STATE_HOME")?.join("wezterm-attention"));
    }
    let home = env
        .get("HOME")
        .ok_or_else(|| AttentionError::new("record_invalid", "HOME is missing"))?;
    Ok(absolute_path(home, "HOME")?.join(".local/state/wezterm-attention"))
}

fn absolute_path(value: &str, name: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > manifest()?.limits.path_max_bytes
        || value
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
        || !Path::new(value).is_absolute()
    {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{name} must be an absolute safe path"),
        ));
    }
    Ok(PathBuf::from(value))
}

pub fn pane_path(root: &Path, address: &PaneAddress) -> PathBuf {
    root.join("v2/realms")
        .join(&address.realm_id)
        .join("incarnations")
        .join(&address.incarnation_id)
        .join("panes")
        .join(&address.pane_id)
}

pub fn launch_path(root: &Path, address: &PaneAddress, launch_id: &str) -> PathBuf {
    pane_path(root, address).join("launches").join(launch_id)
}

pub fn mkdir_private(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            AttentionError::new("state_permissions", "state path has no existing ancestor")
        })?;
    }
    for directory in missing.iter().rev() {
        fs::create_dir(directory).map_err(|_| {
            AttentionError::new("state_permissions", "state directory could not be created")
        })?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|_| {
            AttentionError::new(
                "state_permissions",
                "state directory permissions could not be set",
            )
        })?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
        AttentionError::new(
            "state_permissions",
            "state directory permissions could not be set",
        )
    })
}

fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).map_err(AttentionError::record_json)?;
    bytes.push(b'\n');
    Ok(bytes)
}

// On Apple platforms Rust maps File::sync_all to F_FULLFSYNC. The Python
// implementation uses POSIX fsync for both files and parent directories, so
// call that syscall directly to retain the same durability boundary.
fn sync_via_fsync(file: &File) -> std::io::Result<()> {
    let result = unsafe { libc::fsync(file.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn sync_parent_directory_with(
    parent: &Path,
    sync: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<()> {
    let Ok(directory) = File::open(parent) else {
        return Ok(());
    };
    sync(&directory).map_err(|_| {
        AttentionError::new(
            "state_permissions",
            "state directory could not be made durable",
        )
    })
}

fn sync_parent_directory(parent: &Path) -> Result<()> {
    sync_parent_directory_with(parent, sync_via_fsync)
}

pub fn read_record(
    path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
) -> Result<Option<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(AttentionError::new(
                "record_invalid",
                "state record could not be read",
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take((manifest()?.limits.max_json_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| AttentionError::new("record_invalid", "state record could not be read"))?;
    if bytes.len() > manifest()?.limits.max_json_bytes {
        return Err(AttentionError::new(
            "record_invalid",
            "state record exceeds its bound",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AttentionError::new("record_invalid", "state record is invalid"))?;
    if let Some(kind) = expected_kind {
        validate_record(&value, Some(kind))?;
    } else if !value.is_object() {
        return Err(AttentionError::new(
            "record_invalid",
            "state value is not an object",
        ));
    }
    expected_identity.validate(&value)?;
    Ok(Some(value))
}

pub fn atomic_replace(path: &Path, value: &Value) -> Result<()> {
    if value.get("kind").is_some() {
        validate_record(value, value.get("kind").and_then(Value::as_str))?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| AttentionError::new("state_permissions", "state path has no parent"))?;
    mkdir_private(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("record"),
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| {
                AttentionError::new(
                    "state_permissions",
                    "temporary state record could not be created",
                )
            })?;
        file.write_all(&canonical_json(value)?)
            .and_then(|()| sync_via_fsync(&file))
            .map_err(|_| {
                AttentionError::new(
                    "state_permissions",
                    "state record could not be made durable",
                )
            })?;
        fs::rename(&temporary, path).map_err(|_| {
            AttentionError::new("state_permissions", "state record could not be replaced")
        })?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
            AttentionError::new(
                "state_permissions",
                "state record permissions could not be set",
            )
        })?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn atomic_replace_if_different(path: &Path, value: &Value) -> Result<bool> {
    let wanted = canonical_json(value)?;
    if let Ok(existing) = fs::read(path)
        && existing == wanted
    {
        if value.get("kind").is_some() {
            validate_record(value, value.get("kind").and_then(Value::as_str))?;
        }
        return Ok(false);
    }
    if path.exists() {
        let identity = RecordIdentity::from_record(value);
        let _ = read_record(path, value.get("kind").and_then(Value::as_str), &identity)?;
    }
    atomic_replace(path, value)?;
    Ok(true)
}

pub fn remove_file_durable(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_parent_directory(parent)?;
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(AttentionError::new(
            "state_permissions",
            "state record could not be removed",
        )),
    }
}

fn remove_path_durable(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|_| {
                AttentionError::new("state_permissions", "state directory could not be removed")
            })?;
            if let Some(parent) = path.parent() {
                sync_parent_directory(parent)?;
            }
            Ok(true)
        }
        Ok(_) => remove_file_durable(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(AttentionError::new(
            "state_permissions",
            "state path could not be inspected",
        )),
    }
}

pub fn with_lock<T>(
    path: &Path,
    timeout: Duration,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let parent = path
        .parent()
        .ok_or_else(|| AttentionError::new("state_permissions", "lock path has no parent"))?;
    mkdir_private(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| AttentionError::new("state_permissions", "state lock could not be opened"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(AttentionError::new(
                        "probe_unavailable",
                        "state lock timed out",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(AttentionError::new(
                    "probe_unavailable",
                    "state lock is unavailable",
                ));
            }
        }
    }
    let result = operation();
    let _ = file.unlock();
    result
}

pub fn commit<T>(
    lock_path: &Path,
    read_path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
    timeout: Duration,
    decide: impl FnOnce(Option<Value>) -> Result<CommitPlan<T>>,
) -> Result<T> {
    let (result, ()) = commit_with(
        lock_path,
        read_path,
        expected_kind,
        expected_identity,
        timeout,
        decide,
        |_| Ok(()),
    )?;
    Ok(result)
}

pub fn commit_with<T, P>(
    lock_path: &Path,
    read_path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
    timeout: Duration,
    decide: impl FnOnce(Option<Value>) -> Result<CommitPlan<T>>,
    after_apply: impl FnOnce(&T) -> Result<P>,
) -> Result<(T, P)> {
    with_lock(lock_path, timeout, || {
        let current = read_record(read_path, expected_kind, expected_identity)?;
        let plan = decide(current)?;
        apply_plan(&plan)?;
        let post_result = after_apply(&plan.result)?;
        Ok((plan.result, post_result))
    })
}

fn apply_plan<T>(plan: &CommitPlan<T>) -> Result<()> {
    for directory in &plan.private_dirs {
        mkdir_private(directory)?;
    }
    for replacement in &plan.replacements {
        if replacement.only_if_different {
            atomic_replace_if_different(&replacement.path, &replacement.value)?;
        } else {
            atomic_replace(&replacement.path, &replacement.value)?;
        }
    }
    for path in &plan.removals {
        remove_path_durable(path)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn commit_nested_with<T, P>(
    outer_lock: &Path,
    inner_lock: &Path,
    read_path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
    timeout: Duration,
    decide: impl FnOnce(Option<Value>) -> Result<CommitPlan<T>>,
    after_apply: impl FnOnce(&T) -> Result<P>,
) -> Result<(T, P)> {
    with_lock(outer_lock, timeout, || {
        with_lock(inner_lock, timeout, || {
            let current = read_record(read_path, expected_kind, expected_identity)?;
            let plan = decide(current)?;
            apply_plan(&plan)?;
            let post_result = after_apply(&plan.result)?;
            Ok((plan.result, post_result))
        })
    })
}

#[allow(clippy::too_many_arguments)]
pub fn commit_triple_with<T, P>(
    outer_lock: &Path,
    middle_lock: &Path,
    inner_lock: &Path,
    read_path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
    timeout: Duration,
    decide: impl FnOnce(Option<Value>) -> Result<CommitPlan<T>>,
    after_apply: impl FnOnce(&T) -> Result<P>,
) -> Result<(T, P)> {
    with_lock(outer_lock, timeout, || {
        with_lock(middle_lock, timeout, || {
            with_lock(inner_lock, timeout, || {
                let current = read_record(read_path, expected_kind, expected_identity)?;
                let plan = decide(current)?;
                apply_plan(&plan)?;
                let post_result = after_apply(&plan.result)?;
                Ok((plan.result, post_result))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;

    use super::sync_parent_directory_with;

    #[test]
    fn parent_directory_sync_failure_is_reported() {
        let error = sync_parent_directory_with(Path::new("/tmp"), |_| {
            Err(io::Error::from_raw_os_error(libc::EIO))
        })
        .expect_err("directory fsync failure must propagate");
        assert_eq!(error.diagnostic.code, "state_permissions");
    }
}
