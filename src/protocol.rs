use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const EMBEDDED_MANIFEST: &str = include_str!("../protocol/v2.json");
pub const EMITTED_DIAGNOSTIC_CODES: [&str; 13] = [
    "identity_unpublished",
    "claim_stale",
    "unsafe_tty",
    "realm_unavailable",
    "incarnation_changed",
    "record_invalid",
    "future_schema",
    "binding_conflict",
    "probe_unavailable",
    "clock_skew",
    "integration_version_mismatch",
    "state_permissions",
    "bad_usage",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub context: BTreeMap<String, Value>,
    pub help: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttentionError {
    pub diagnostic: Diagnostic,
    pub exit_code: i32,
}

impl AttentionError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        assert!(
            EMITTED_DIAGNOSTIC_CODES.contains(&code),
            "undeclared diagnostic code: {code}"
        );
        Self {
            diagnostic: Diagnostic {
                code: code.to_owned(),
                message: message.into(),
                context: BTreeMap::new(),
                help: "attention doctor".to_owned(),
            },
            exit_code: 3,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        let mut error = Self::new("bad_usage", message);
        error.exit_code = 2;
        error
    }

    pub fn record_json(error: serde_json::Error) -> Self {
        Self::new("record_invalid", format!("JSON is invalid: {error}"))
    }
}

impl fmt::Display for AttentionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {}",
            self.diagnostic.code, self.diagnostic.message
        )
    }
}

impl std::error::Error for AttentionError {}

pub type Result<T> = std::result::Result<T, AttentionError>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_schema: u64,
    pub wire_version: u64,
    pub record_schema: u64,
    pub writer_version: String,
    pub digests: DigestRecipes,
    pub limits: Limits,
    pub enums: Enums,
    pub wire: ShapeSpec,
    pub records: BTreeMap<String, ShapeSpec>,
    pub lifecycle_shapes: BTreeMap<String, ShapeSpec>,
    pub lifecycle_variants: BTreeMap<String, ShapeSpec>,
    pub lifecycle_enums: BTreeMap<String, BTreeSet<String>>,
    pub lifecycle_sources: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestRecipes {
    pub algorithm: String,
    pub encoding: String,
    pub agent_key_input: String,
    pub owner_key_input: String,
    pub realm_id_input: String,
    pub incarnation_id_input: String,
    pub tty_fingerprint_input: String,
    pub binding_id_input: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub pane_id_max_digits: usize,
    pub canonical_decimal_max_digits: usize,
    pub safe_label_max_bytes: usize,
    pub path_max_bytes: usize,
    pub model_max_bytes: usize,
    pub writer_version_max_bytes: usize,
    pub ttl_ms_max: u64,
    pub frame_max: u64,
    pub subagent_ttl_ms: u64,
    pub max_json_bytes: usize,
    pub lifecycle_max_json_bytes: usize,
    pub lifecycle_max_depth: usize,
    pub lifecycle_pool_max_count: usize,
    pub lifecycle_pool_max_bytes: usize,
    pub lifecycle_envelope_max_bytes: usize,
    pub lifecycle_observation_max_bytes: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enums {
    pub activity_types: BTreeSet<String>,
    pub binding_health: BTreeSet<String>,
    pub binding_phase: BTreeSet<String>,
    pub diagnostic_codes: BTreeSet<String>,
    pub end_reasons: BTreeSet<String>,
    pub pane_presence: BTreeSet<String>,
    pub providers: BTreeSet<String>,
    pub reader_confidence: BTreeSet<String>,
    pub subagent_providers: BTreeSet<String>,
    pub subagent_statuses: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSpec {
    pub required: BTreeSet<String>,
    pub optional: BTreeSet<String>,
    pub types: BTreeMap<String, FieldType>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    LifecyclePools,
    ObservationPool,
    ObservationArray,
    LifecycleActor,
    NativeCorrelation,
    ToolClass,
    QuestionMode,
    ResultSurface,
    AttemptOutcome,
    ElicitationMode,
    SelectionAction,
    NoticeSubtype,
    PolicyScope,
    InputSource,
    ErrorCategory,
    CompactionTrigger,
    RecordKind,
    RecordSchema,
    WireVersion,
    DecimalNs20,
    MonotonicNs20,
    UnixNs20,
    Hex64,
    Uuid,
    CanonicalDecimal,
    PaneAddress,
    ActivityTarget,
    AbsolutePath,
    SafeLabel,
    Model,
    WriterVersion,
    Boolean,
    NonnegativeInteger,
    PositiveInteger,
    SubagentTtlMs,
    Provider,
    SubagentProvider,
    SubagentStatus,
    ActivityType,
    EndReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Valid,
    FutureSchema,
    RecordInvalid,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::FutureSchema => "future_schema",
            Self::RecordInvalid => "record_invalid",
        }
    }
}

static MANIFEST: OnceLock<std::result::Result<Manifest, String>> = OnceLock::new();

pub fn parse_manifest(source: &str) -> Result<Manifest> {
    let parsed: Manifest = serde_json::from_str(source).map_err(|error| {
        AttentionError::new(
            "integration_version_mismatch",
            format!("manifest is invalid: {error}"),
        )
    })?;
    if parsed.manifest_schema != 1 {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "manifest schema is unsupported",
        ));
    }
    if parsed.digests.algorithm != "sha256" || parsed.digests.encoding != "lowercase_hex" {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "manifest digest algorithm is unsupported",
        ));
    }
    for (name, spec) in std::iter::once(("wire", &parsed.wire)).chain(
        parsed
            .records
            .iter()
            .map(|(name, spec)| (name.as_str(), spec)),
    ) {
        let declared: BTreeSet<_> = spec.required.union(&spec.optional).cloned().collect();
        let typed: BTreeSet<_> = spec.types.keys().cloned().collect();
        if declared != typed {
            return Err(AttentionError::new(
                "integration_version_mismatch",
                format!("manifest shape {name} has inconsistent fields"),
            ));
        }
    }
    Ok(parsed)
}

pub fn manifest() -> Result<&'static Manifest> {
    match MANIFEST
        .get_or_init(|| parse_manifest(EMBEDDED_MANIFEST).map_err(|error| error.to_string()))
    {
        Ok(value) => Ok(value),
        Err(message) => Err(AttentionError::new(
            "integration_version_mismatch",
            message.clone(),
        )),
    }
}

fn safe_text(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty()
            && text.len() <= maximum
            && !text
                .chars()
                .any(|character| character < ' ' || character == '\u{7f}')
    })
}

fn hex64(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn decimal_ns20(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.len() == 20 && text.bytes().all(|byte| byte.is_ascii_digit()))
}

fn canonical_decimal(value: &Value, maximum: usize) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty()
            && text.len() <= maximum
            && text.bytes().all(|byte| byte.is_ascii_digit())
            && (text == "0" || !text.starts_with('0'))
    })
}

fn validate_address(value: &Value, protocol: &Manifest) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == 3
        && object.contains_key("realm_id")
        && object.contains_key("incarnation_id")
        && object.contains_key("pane_id")
        && hex64(&object["realm_id"])
        && hex64(&object["incarnation_id"])
        && canonical_decimal(&object["pane_id"], protocol.limits.pane_id_max_digits)
}

fn validate_target(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match object.get("kind").and_then(Value::as_str) {
        Some("launch") => object.len() == 1,
        Some("binding") => {
            object.len() == 2 && object.contains_key("binding_id") && hex64(&object["binding_id"])
        }
        _ => false,
    }
}

fn validate_field(
    field_type: FieldType,
    value: &Value,
    protocol: &Manifest,
    expected_kind: Option<&str>,
) -> bool {
    let limits = &protocol.limits;
    match field_type {
        FieldType::LifecyclePools | FieldType::ObservationPool | FieldType::NativeCorrelation => {
            let shape = match field_type {
                FieldType::LifecyclePools => "pools",
                FieldType::ObservationPool => "pool",
                _ => "correlation",
            };
            protocol
                .lifecycle_shapes
                .get(shape)
                .is_some_and(|spec| validate_shape(value, spec, protocol, None))
        }
        FieldType::LifecycleActor => {
            value
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    ["lead", "child"].contains(&kind)
                        && protocol
                            .lifecycle_shapes
                            .get(kind)
                            .is_some_and(|spec| validate_shape(value, spec, protocol, Some(kind)))
                })
        }
        FieldType::ObservationArray => value.as_array().is_some_and(|items| {
            items.len() <= protocol.limits.lifecycle_pool_max_count
                && items.iter().all(|item| {
                    item.get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| {
                            protocol.lifecycle_variants.get(kind).is_some_and(|spec| {
                                validate_shape(item, spec, protocol, Some(kind))
                            })
                        })
                })
        }),
        FieldType::ToolClass
        | FieldType::QuestionMode
        | FieldType::ResultSurface
        | FieldType::AttemptOutcome
        | FieldType::ElicitationMode
        | FieldType::SelectionAction
        | FieldType::NoticeSubtype
        | FieldType::PolicyScope
        | FieldType::InputSource
        | FieldType::ErrorCategory
        | FieldType::CompactionTrigger => {
            let name = match field_type {
                FieldType::ToolClass => "tool_class",
                FieldType::QuestionMode => "question_mode",
                FieldType::ResultSurface => "result_surface",
                FieldType::AttemptOutcome => "attempt_outcome",
                FieldType::ElicitationMode => "elicitation_mode",
                FieldType::SelectionAction => "selection_action",
                FieldType::NoticeSubtype => "notice_subtype",
                FieldType::PolicyScope => "policy_scope",
                FieldType::InputSource => "input_source",
                FieldType::ErrorCategory => "error_category",
                _ => "compaction_trigger",
            };
            value.as_str().is_some_and(|text| {
                protocol
                    .lifecycle_enums
                    .get(name)
                    .is_some_and(|values| values.contains(text))
            })
        }
        FieldType::RecordKind => value.as_str() == expected_kind,
        FieldType::RecordSchema => value.as_u64() == Some(protocol.record_schema),
        FieldType::WireVersion => value.as_u64() == Some(protocol.wire_version),
        FieldType::DecimalNs20 | FieldType::MonotonicNs20 | FieldType::UnixNs20 => {
            decimal_ns20(value)
        }
        FieldType::Hex64 => hex64(value),
        FieldType::Uuid => value.as_str().is_some_and(|text| {
            Uuid::parse_str(text).is_ok_and(|parsed| parsed.to_string() == text)
        }),
        FieldType::CanonicalDecimal => {
            canonical_decimal(value, limits.canonical_decimal_max_digits)
        }
        FieldType::PaneAddress => validate_address(value, protocol),
        FieldType::ActivityTarget => validate_target(value),
        FieldType::AbsolutePath => {
            safe_text(value, limits.path_max_bytes)
                && value
                    .as_str()
                    .is_some_and(|text| Path::new(text).is_absolute())
        }
        FieldType::SafeLabel => safe_text(value, limits.safe_label_max_bytes),
        FieldType::Model => safe_text(value, limits.model_max_bytes),
        FieldType::WriterVersion => safe_text(value, limits.writer_version_max_bytes),
        FieldType::Boolean => value.is_boolean(),
        FieldType::NonnegativeInteger => value
            .as_u64()
            .is_some_and(|number| number <= limits.frame_max),
        FieldType::PositiveInteger => value
            .as_u64()
            .is_some_and(|number| number > 0 && number <= limits.ttl_ms_max),
        FieldType::SubagentTtlMs => value.as_u64() == Some(limits.subagent_ttl_ms),
        FieldType::Provider => value
            .as_str()
            .is_some_and(|item| protocol.enums.providers.contains(item)),
        FieldType::SubagentProvider => value
            .as_str()
            .is_some_and(|item| protocol.enums.subagent_providers.contains(item)),
        FieldType::SubagentStatus => value
            .as_str()
            .is_some_and(|item| protocol.enums.subagent_statuses.contains(item)),
        FieldType::ActivityType => value
            .as_str()
            .is_some_and(|item| protocol.enums.activity_types.contains(item)),
        FieldType::EndReason => value
            .as_str()
            .is_some_and(|item| protocol.enums.end_reasons.contains(item)),
    }
}

fn validate_shape(
    value: &Value,
    spec: &ShapeSpec,
    protocol: &Manifest,
    expected_kind: Option<&str>,
) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !spec.required.iter().all(|field| object.contains_key(field)) {
        return false;
    }
    object.iter().all(|(field, value)| {
        (spec.required.contains(field) || spec.optional.contains(field))
            && spec.types.get(field).is_some_and(|field_type| {
                validate_field(*field_type, value, protocol, expected_kind)
            })
    })
}

pub fn parse_wire_value(value: &Value, protocol: &Manifest) -> Verdict {
    if value
        .get("wire")
        .and_then(Value::as_u64)
        .is_some_and(|version| version > protocol.wire_version)
    {
        return Verdict::FutureSchema;
    }
    if validate_shape(value, &protocol.wire, protocol, None) {
        Verdict::Valid
    } else {
        Verdict::RecordInvalid
    }
}

pub fn parse_record_value(value: &Value, protocol: &Manifest) -> Verdict {
    if value
        .get("schema")
        .and_then(Value::as_u64)
        .is_some_and(|schema| schema > protocol.record_schema)
    {
        return Verdict::FutureSchema;
    }
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return Verdict::RecordInvalid;
    };
    let Some(spec) = protocol.records.get(kind) else {
        return Verdict::RecordInvalid;
    };
    if !validate_shape(value, spec, protocol, Some(kind)) {
        return Verdict::RecordInvalid;
    }
    let digest_matches = match kind {
        "lifecycle_snapshot" => crate::observations::LifecycleSnapshot::deserialize(value)
            .is_ok_and(|snapshot| snapshot.validate_semantics().is_ok()),
        "subagent_presence" => value
            .get("agent_id")
            .and_then(Value::as_str)
            .zip(value.get("agent_key").and_then(Value::as_str))
            .is_some_and(|(id, key)| sha256_hex(id.as_bytes()) == key),
        "review" => value
            .get("owner_id")
            .and_then(Value::as_str)
            .zip(value.get("owner_key").and_then(Value::as_str))
            .is_some_and(|(id, key)| sha256_hex(id.as_bytes()) == key),
        _ => true,
    };
    if digest_matches {
        Verdict::Valid
    } else {
        Verdict::RecordInvalid
    }
}

pub fn validate_record(value: &Value, expected_kind: Option<&str>) -> Result<()> {
    let protocol = manifest()?;
    match parse_record_value(value, protocol) {
        Verdict::Valid
            if expected_kind
                .is_none_or(|kind| value.get("kind").and_then(Value::as_str) == Some(kind)) =>
        {
            Ok(())
        }
        Verdict::FutureSchema => Err(AttentionError::new(
            "future_schema",
            "record schema is unsupported",
        )),
        _ => Err(AttentionError::new(
            "record_invalid",
            "state record is invalid",
        )),
    }
}

/// Scan containers before recursive JSON decoding. Syntax remains serde's job.
pub fn bounded_lifecycle_json(bytes: &[u8]) -> bool {
    let Ok(protocol) = manifest() else {
        return false;
    };
    if bytes.len() > protocol.limits.lifecycle_max_json_bytes {
        return false;
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > protocol.limits.lifecycle_max_depth {
                        return false;
                    }
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
    }
    depth == 0 && !quoted
}

pub fn eligible_subagent_presence(
    presence: &Value,
    clear_order: Option<&str>,
    floor_order: Option<&str>,
    now_unix_ns: Option<&str>,
    protocol: &Manifest,
) -> (bool, Option<&'static str>) {
    if parse_record_value(presence, protocol) != Verdict::Valid {
        return (false, Some("record_invalid"));
    }
    let Some(now) = now_unix_ns.filter(|value| decimal_ns20(&Value::String((*value).to_owned())))
    else {
        return (false, Some("probe_unavailable"));
    };
    let order = presence["observed_mono_ns"].as_str().unwrap_or("");
    if presence["status"] != "active"
        || clear_order.is_some_and(|clear| order <= clear)
        || floor_order.is_some_and(|floor| order <= floor)
    {
        return (false, None);
    }
    let written = presence["written_at_unix_ns"].as_str().unwrap_or("");
    if now < written {
        return (false, Some("clock_skew"));
    }
    let now = now.parse::<u128>().expect("validated decimal");
    let written = written.parse::<u128>().expect("validated record decimal");
    let ttl = presence["ttl_ms"].as_u64().unwrap_or(0) as u128 * 1_000_000;
    (now <= written + ttl, None)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
