use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::identity::PaneAddress;
use crate::protocol::{AttentionError, Result, free_of_control, manifest, validate_record};

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
        if parts.first() == Some(&Some("v2")) && parts.get(1) == Some(&Some("sessions")) {
            // The index names each entry by what it holds, which the reader
            // checks; the path fixes no field of the record.
            let shaped = match kind {
                "session_index" => parts.len() == 3 && parts[2] == Some("complete.json"),
                "session_binding" => parts.len() == 4,
                _ => false,
            };
            return if shaped {
                Ok(Self::unscoped())
            } else {
                Err(invalid())
            };
        }
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
            "lifecycle_snapshot" => "lifecycle.json",
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

/// The variables that can name the state root, in the order they decide it.
pub(crate) const STATE_ROOT_VARIABLES: [&str; 2] = ["WEZTERM_ATTENTION_DIR", "XDG_STATE_HOME"];

/// What [`crate::environment`] gives a state-root variable whose value is not
/// UTF-8 and could decide the root. The environment is handed on as text, and a value it cannot hold
/// still has to decide the root, as it does for the plugin, which reads the
/// raw bytes; no path holds a NUL, so this can stand for nothing else.
pub(crate) const NOT_UTF8: &str = "\0";

/// The plugin and the Pi extension resolve the same root in the same order.
/// An empty WEZTERM_ATTENTION_DIR counts as unset; any other value must be a
/// safe absolute path. XDG_STATE_HOME is used only when it is one, because the
/// XDG spec says a relative or empty value is to be ignored. Either one that
/// is not UTF-8 is refused where it decides the root: this writer cannot name
/// that directory, and another root would hide every record from the plugin.
pub fn state_root(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(name) = STATE_ROOT_VARIABLES
        .into_iter()
        .find(|name| env.get(*name).is_some_and(|value| !value.is_empty()))
        && env[name] == NOT_UTF8
    {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{name} is not UTF-8"),
        ));
    }
    if let Some(path) = env
        .get("WEZTERM_ATTENTION_DIR")
        .filter(|path| !path.is_empty())
    {
        return absolute_path(path, "WEZTERM_ATTENTION_DIR");
    }
    if let Some(path) = env
        .get("XDG_STATE_HOME")
        .and_then(|path| absolute_path(path, "XDG_STATE_HOME").ok())
    {
        return Ok(path.join("wezterm-attention"));
    }
    let home = env
        .get("HOME")
        .ok_or_else(|| AttentionError::new("record_invalid", "HOME is missing"))?;
    Ok(absolute_path(home, "HOME")?.join(".local/state/wezterm-attention"))
}

fn absolute_path(value: &str, name: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > manifest()?.limits.path_max_bytes
        || !free_of_control(value)
        || !Path::new(value).is_absolute()
    {
        return Err(AttentionError::new(
            "record_invalid",
            format!("{name} must be an absolute safe path"),
        ));
    }
    Ok(PathBuf::from(value))
}

pub fn realm_path(root: &Path, realm_id: &str) -> PathBuf {
    root.join("v2/realms").join(realm_id)
}

pub fn incarnation_path(root: &Path, realm_id: &str, incarnation_id: &str) -> PathBuf {
    realm_path(root, realm_id)
        .join("incarnations")
        .join(incarnation_id)
}

pub fn pane_path(root: &Path, address: &PaneAddress) -> PathBuf {
    incarnation_path(root, &address.realm_id, &address.incarnation_id)
        .join("panes")
        .join(&address.pane_id)
}

pub fn launch_path(root: &Path, address: &PaneAddress, launch_id: &str) -> PathBuf {
    pane_path(root, address).join("launches").join(launch_id)
}

/// Where a binding record is kept. Below an empty root it is the path the
/// session index keys the binding's entry by.
pub fn binding_path(
    root: &Path,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> PathBuf {
    launch_path(root, address, launch_id)
        .join("bindings")
        .join(binding_id)
        .join("binding.json")
}

/// The session index: `v2/sessions/<session key>/<entry key>.json` names
/// each binding of one provider session, so finding a session's other
/// bindings reads one directory instead of walking every binding. The keys
/// are the manifest's `session_key_input` and `session_entry_key_input`
/// digests. It is derived state: a binding is written with its entry in the
/// same commit, and never depends on it.
///
/// A reader trusts the index only while `v2/sessions/complete.json` says it
/// holds every binding. A store that had bindings before its writer wrote
/// entries has none until `sweep --apply` has written an entry for each.
pub fn session_dir(root: &Path, provider: &str, provider_session_id: &str) -> PathBuf {
    let mut input = provider.as_bytes().to_vec();
    input.push(0);
    input.extend_from_slice(provider_session_id.as_bytes());
    root.join("v2/sessions")
        .join(crate::protocol::sha256_hex(&input))
}

/// Where the session index names one binding: under its session, by the
/// digest of the binding record's path below the state root, which no other
/// binding shares.
pub fn session_entry_path(
    root: &Path,
    provider: &str,
    provider_session_id: &str,
    address: &PaneAddress,
    launch_id: &str,
    binding_id: &str,
) -> PathBuf {
    let binding = binding_path(Path::new(""), address, launch_id, binding_id);
    let key = crate::protocol::sha256_hex(binding.to_string_lossy().as_bytes());
    session_dir(root, provider, provider_session_id).join(format!("{key}.json"))
}

/// A binding record's session index entry and where it goes.
pub fn binding_session_entry(root: &Path, binding: &Value) -> Result<(PathBuf, Value)> {
    let invalid = || AttentionError::new("record_invalid", "binding record is invalid");
    let text = |field: &str| {
        binding
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(invalid)
    };
    let address: PaneAddress = binding
        .get("address")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or_else(invalid)?;
    let (launch_id, binding_id) = (text("launch_id")?, text("binding_id")?);
    Ok((
        session_entry_path(
            root,
            text("provider")?,
            text("provider_session_id")?,
            &address,
            launch_id,
            binding_id,
        ),
        session_entry(&address, launch_id, binding_id)?,
    ))
}

/// The session index entry for one binding.
pub fn session_entry(address: &PaneAddress, launch_id: &str, binding_id: &str) -> Result<Value> {
    Ok(serde_json::json!({
        "kind": "session_binding",
        "schema": manifest()?.record_schema,
        "address": address,
        "launch_id": launch_id,
        "binding_id": binding_id,
    }))
}

/// The record that says the session index holds every binding.
pub fn session_index_path(root: &Path) -> PathBuf {
    root.join("v2/sessions/complete.json")
}

pub fn session_index_marker() -> Result<Value> {
    Ok(serde_json::json!({"kind": "session_index", "schema": manifest()?.record_schema}))
}

/// Whether an end record ends this binding record, the one rule every reader
/// and writer applies.
///
/// An end ends the binding event it names in `binding_event_id`, whatever the
/// clocks say: the monotonic clock restarts at boot, so an end sweep writes
/// after a reboot carries a smaller stamp than a binding recorded before it.
/// An end observed at or after the binding ends it too, which covers an end
/// that names no event and one whose event raced a resumed start. Any other
/// end belongs to an earlier binding of the same id, which a resume replaced.
pub fn ends_binding(end: &Value, binding: &Value) -> bool {
    let named = end
        .get("binding_event_id")
        .and_then(Value::as_str)
        .is_some_and(|event| binding.get("event_id").and_then(Value::as_str) == Some(event));
    let observed = |record: &Value| {
        record
            .get("observed_mono_ns")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    named
        || observed(end)
            .zip(observed(binding))
            .is_some_and(|(end, binding)| end >= binding)
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
        // Created private rather than chmod-ed afterwards, so it is never
        // briefly open to the umask. Another writer creating the same
        // directory first, as two claims after a mux restart do, is success.
        match DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists && directory.is_dir() => {}
            Err(_) => {
                return Err(AttentionError::new(
                    "state_permissions",
                    "state directory could not be created",
                ));
            }
        }
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
    match read_record_typed(path, expected_kind, expected_identity) {
        RecordRead::Present(value) => Ok(Some(value)),
        RecordRead::Missing => Ok(None),
        RecordRead::Unavailable(error)
        | RecordRead::Invalid(error)
        | RecordRead::Unsupported(error) => Err(error),
    }
}

/// Read evidence without turning an I/O failure into invalid bytes or absence.
#[derive(Clone, Debug)]
pub enum RecordRead {
    Present(Value),
    Missing,
    Unavailable(AttentionError),
    Invalid(AttentionError),
    Unsupported(AttentionError),
}

/// Injectable filesystem boundary for scoped, read-only fact assembly.
pub trait RecordReader {
    fn read(&self, path: &Path, kind: Option<&str>, identity: &RecordIdentity) -> RecordRead;
    fn entries(&self, directory: &Path) -> Result<Vec<PathBuf>>;
}

pub struct FileRecords;

impl RecordReader for FileRecords {
    fn read(&self, path: &Path, kind: Option<&str>, identity: &RecordIdentity) -> RecordRead {
        read_record_typed(path, kind, identity)
    }

    fn entries(&self, directory: &Path) -> Result<Vec<PathBuf>> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => {
                return Err(AttentionError::new(
                    "probe_unavailable",
                    "record directory could not be enumerated",
                ));
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| {
                AttentionError::new("probe_unavailable", "record directory entry is unavailable")
            })?;
            if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let kind = entry.file_type().map_err(|_| {
                AttentionError::new("probe_unavailable", "record entry type is unavailable")
            })?;
            if kind.is_symlink() {
                return Err(AttentionError::new(
                    "record_invalid",
                    "record collection contains a symlink",
                ));
            }
            paths.push(entry.path());
        }
        paths.sort();
        Ok(paths)
    }
}

pub fn read_record_typed(
    path: &Path,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
) -> RecordRead {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return RecordRead::Missing,
        Err(_) => {
            return RecordRead::Unavailable(AttentionError::new(
                "probe_unavailable",
                "state record could not be read",
            ));
        }
    };
    let mut bytes = Vec::new();
    let protocol = match manifest() {
        Ok(protocol) => protocol,
        Err(error) => return RecordRead::Unsupported(error),
    };
    let maximum = if expected_kind == Some("lifecycle_snapshot") {
        protocol.limits.lifecycle_max_json_bytes
    } else {
        protocol.limits.max_json_bytes
    };
    if file
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return RecordRead::Unavailable(AttentionError::new(
            "probe_unavailable",
            "state record could not be read",
        ));
    }
    match decode_record(&bytes, maximum, expected_kind, expected_identity) {
        Ok(value) => RecordRead::Present(value),
        Err(error) if error.diagnostic.code == "future_schema" => RecordRead::Unsupported(error),
        Err(error) => RecordRead::Invalid(error),
    }
}

fn decode_record(
    bytes: &[u8],
    maximum: usize,
    expected_kind: Option<&str>,
    expected_identity: &RecordIdentity,
) -> Result<Value> {
    if bytes.len() > maximum {
        return Err(AttentionError::new(
            "record_invalid",
            "state record exceeds its bound",
        ));
    }
    if expected_kind == Some("lifecycle_snapshot")
        && !crate::protocol::bounded_lifecycle_json(bytes)
    {
        return Err(AttentionError::new(
            "record_invalid",
            "lifecycle JSON nesting exceeds its bound",
        ));
    }
    let value: Value = serde_json::from_slice(bytes)
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
    Ok(value)
}

pub fn atomic_replace(path: &Path, value: &Value) -> Result<()> {
    PreparedRecordWrite::new(path.to_owned(), value)?.apply()
}

/// Validated, immutable bytes prepared before applying other outputs of a mutation.
#[derive(Clone, Debug)]
pub(crate) struct PreparedRecordWrite {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl PreparedRecordWrite {
    pub(crate) fn new(path: PathBuf, value: &Value) -> Result<Self> {
        if value.get("kind").is_some() {
            validate_record(value, value.get("kind").and_then(Value::as_str))?;
        }
        Ok(Self {
            path,
            bytes: canonical_json(value)?,
        })
    }

    pub(crate) fn apply(&self) -> Result<()> {
        atomic_replace_bytes(&self.path, &self.bytes)
    }
}

fn atomic_replace_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
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
        file.write_all(bytes)
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
    if path.exists() && value.get("kind").is_some() {
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

    use std::collections::BTreeMap;

    use super::{PreparedRecordWrite, ends_binding, state_root, sync_parent_directory_with};

    #[test]
    fn an_end_ends_the_binding_it_names_or_one_it_was_observed_after() {
        let binding = serde_json::json!({
            "event_id": "00000000-0000-4000-8000-000000000001",
            "observed_mono_ns": "00000000000000000500",
        });
        let end = |event: Option<&str>, observed: &str| {
            let mut end = serde_json::json!({"observed_mono_ns": observed});
            if let Some(event) = event {
                end["binding_event_id"] = serde_json::json!(event);
            }
            end
        };
        let named = Some("00000000-0000-4000-8000-000000000001");
        let other = Some("00000000-0000-4000-8000-000000000002");
        // Written after a reboot: a smaller stamp, and the binding's own event.
        assert!(ends_binding(&end(named, "00000000000000000100"), &binding));
        assert!(ends_binding(&end(None, "00000000000000000500"), &binding));
        assert!(ends_binding(&end(other, "00000000000000000600"), &binding));
        // An earlier binding of the same id, which a resume replaced.
        assert!(!ends_binding(&end(other, "00000000000000000100"), &binding));
        assert!(!ends_binding(&end(None, "00000000000000000499"), &binding));
    }

    #[test]
    fn concurrent_writers_creating_one_directory_all_succeed() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Barrier};
        let root = std::env::temp_dir().join(format!("attention-mkdir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        for attempt in 0..40 {
            let target = root.join(format!("{attempt}/panes/42/launches"));
            let barrier = Arc::new(Barrier::new(8));
            let writers: Vec<_> = (0..8)
                .map(|_| {
                    let (target, barrier) = (target.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        super::mkdir_private(&target)
                    })
                })
                .collect();
            for writer in writers {
                writer
                    .join()
                    .unwrap()
                    .expect("a racing writer still succeeds");
            }
            for directory in [root.join(attempt.to_string()), target] {
                let mode = std::fs::metadata(directory).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o700);
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_root_skips_empty_and_relative_locations_it_may_ignore() {
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect()
        };
        let home = [("HOME", "/home/a")];
        let fallback = Path::new("/home/a/.local/state/wezterm-attention");
        assert_eq!(state_root(&env(&home)).unwrap(), fallback);
        for ignored in ["", "relative/state"] {
            let mut pairs = home.to_vec();
            pairs.push(("XDG_STATE_HOME", ignored));
            assert_eq!(state_root(&env(&pairs)).unwrap(), fallback, "{ignored:?}");
        }
        let mut pairs = home.to_vec();
        pairs.push(("XDG_STATE_HOME", "/xdg"));
        assert_eq!(
            state_root(&env(&pairs)).unwrap(),
            Path::new("/xdg/wezterm-attention")
        );
        pairs.push(("WEZTERM_ATTENTION_DIR", ""));
        assert_eq!(
            state_root(&env(&pairs)).unwrap(),
            Path::new("/xdg/wezterm-attention")
        );
        pairs.pop();
        pairs.push(("WEZTERM_ATTENTION_DIR", "/explicit"));
        assert_eq!(state_root(&env(&pairs)).unwrap(), Path::new("/explicit"));
        pairs.pop();
        pairs.push(("WEZTERM_ATTENTION_DIR", "relative"));
        assert!(state_root(&env(&pairs)).is_err());
    }

    #[test]
    fn prepared_record_freezes_validated_bytes_before_filesystem_effects() {
        let root =
            std::env::temp_dir().join(format!("attention-prepared-{}", uuid::Uuid::new_v4()));
        let path = root.join("lifecycle.json");
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/lifecycle/observations.json"
        ))
        .unwrap();
        let mut value = fixtures["cases"][1]["value"].clone();
        let prepared = PreparedRecordWrite::new(path.clone(), &value).unwrap();
        assert!(!root.exists(), "preparation must not touch the filesystem");
        value["schema"] = serde_json::json!("invalid");
        assert!(PreparedRecordWrite::new(path.clone(), &value).is_err());
        assert!(!root.exists(), "invalid preparation must not create state");
        prepared.apply().unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, fixtures["cases"][1]["value"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parent_directory_sync_failure_is_reported() {
        let error = sync_parent_directory_with(Path::new("/tmp"), |_| {
            Err(io::Error::from_raw_os_error(libc::EIO))
        })
        .expect_err("directory fsync failure must propagate");
        assert_eq!(error.diagnostic.code, "state_permissions");
    }
}
