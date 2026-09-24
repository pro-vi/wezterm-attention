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
/// A closed set of values the contract names, serialized in snake_case.
///
/// Lives here rather than beside the observation model because `parse_manifest`
/// validates the manifest against some of these, and a validator that has to
/// reach up into the model it validates is not a foundation.
macro_rules! vocabulary {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
pub(crate) use vocabulary;

vocabulary!(ToolClass {
    Generic,
    Question,
    Permission
});
vocabulary!(QuestionMode {
    Blocking,
    Nonblocking
});

/// The agents Attention supports. `parse_manifest` checks this against the
/// manifest's own `enums.providers`, so a manifest cannot declare a provider
/// this binary does not implement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    Claude,
    Codex,
    Pi,
}

impl Provider {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }
}

/// Whether a provider's native hook is one Attention asks to be registered.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HookRegistration {
    Register,
    Ignored,
}

/// One native hook the manifest declares for a provider.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHookDeclaration {
    pub native_event: String,
    pub registration: HookRegistration,
}

/// What a hook event did to the state it was given.
///
/// Closed vocabulary: consumers switch on these values, so adding one is a
/// protocol change rather than a local choice. It is an enum because the writer
/// names it at roughly eighty sites, and a misspelling there used to be a
/// silently valid string that no consumer would ever match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Applied,
    Confirmed,
    Replaced,
    Skipped,
    Ignored,
    Conflict,
    Partial,
    RepairedProjection,
}

impl Disposition {
    /// The published spelling. Serialization goes through this too, so the wire
    /// vocabulary has one definition rather than one per derive attribute.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Confirmed => "confirmed",
            Self::Replaced => "replaced",
            Self::Skipped => "skipped",
            Self::Ignored => "ignored",
            Self::Conflict => "conflict",
            Self::Partial => "partial",
            Self::RepairedProjection => "repaired_projection",
        }
    }
}

impl fmt::Display for Disposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Disposition {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Compare against the published spelling, so a test can assert the wire value
/// it actually cares about without naming the Rust variant.
impl PartialEq<&str> for Disposition {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Disposition> for &str {
    fn eq(&self, other: &Disposition) -> bool {
        *self == other.as_str()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_schema: u64,
    pub wire_version: u64,
    pub record_schema: u64,
    pub writer_version: String,
    pub tool_classification: BTreeMap<String, BTreeMap<String, ToolClassification>>,
    pub native_hooks: BTreeMap<String, BTreeMap<String, NativeHookDeclaration>>,
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
pub struct ToolClassification {
    pub tool_class: ToolClass,
    pub question_mode: Option<QuestionMode>,
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
    if parsed.manifest_schema != 2 {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "manifest schema is unsupported",
        ));
    }
    if parsed.native_hooks.keys().cloned().collect::<BTreeSet<_>>() != parsed.enums.providers
        || parsed.native_hooks.iter().any(|(provider, hooks)| {
            Provider::parse(provider).is_none()
                || hooks.is_empty()
                || hooks.iter().any(|(name, declaration)| {
                    name.is_empty()
                        || name.len() > parsed.limits.safe_label_max_bytes
                        || !free_of_control(name)
                        || declaration.native_event.is_empty()
                        || declaration.native_event.len() > parsed.limits.safe_label_max_bytes
                        || !free_of_control(&declaration.native_event)
                })
        })
    {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "manifest native hooks are invalid",
        ));
    }
    if parsed
        .tool_classification
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != parsed.enums.providers
        || parsed.tool_classification.values().any(|tools| {
            tools.iter().any(|(name, class)| {
                name.is_empty()
                    || name.len() > parsed.limits.safe_label_max_bytes
                    || !free_of_control(name)
                    || (class.tool_class == ToolClass::Question) != class.question_mode.is_some()
            })
        })
    {
        return Err(AttentionError::new(
            "integration_version_mismatch",
            "manifest tool classification is invalid",
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

/// Whether text holds no control character: C0 (U+0000-U+001F), DEL and C1
/// (U+0080-U+009F). C1 matters as much as C0 because a terminal reads U+009B
/// as CSI and U+009D as OSC, so a stored C1 byte printed raw can retitle a
/// window, clear the screen or write the clipboard. Every text check in the
/// writer, the plugin and the independent checker uses this one rule.
pub fn free_of_control(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn safe_text(value: &Value, maximum: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.len() <= maximum && free_of_control(text))
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

/// Serialize a document for a terminal to show. serde_json escapes C0 and
/// leaves C1 raw, so a C1 character that entered a record through another
/// writer would reach the terminal as a control sequence. JSON structure is
/// ASCII, so every C1 character in the text sits inside a string and its
/// `\u00XX` escape decodes to the same value.
pub fn printable_json<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    let text = serde_json::to_string(value)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if ('\u{80}'..='\u{9f}').contains(&character) {
            escaped.push_str(&format!("\\u{:04x}", u32::from(character)));
        } else {
            escaped.push(character);
        }
    }
    Ok(escaped)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod consumer_manifest_tests {
    use super::*;
    use crate::observations::classify_tool;

    #[test]
    fn classifications_keep_exact_names_and_reject_mixed_manifests() {
        assert_eq!(
            classify_tool("claude", "AskUserQuestion"),
            (ToolClass::Question, Some(QuestionMode::Blocking))
        );
        assert_eq!(
            classify_tool("codex", "request_user_input_async"),
            (ToolClass::Question, Some(QuestionMode::Nonblocking))
        );
        assert_eq!(
            classify_tool("codex", "request_permissions"),
            (ToolClass::Permission, None)
        );
        for (provider, name) in [
            ("pi", "AskUserQuestion"),
            ("codex", "request_user_input_async_extra"),
            ("unknown", "request_permissions"),
        ] {
            assert_eq!(classify_tool(provider, name), (ToolClass::Generic, None));
        }
        let baseline: Value = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
        let mut old = baseline.clone();
        old["manifest_schema"] = Value::from(1);
        assert!(parse_manifest(&old.to_string()).is_err());
        let mut invalid = baseline;
        invalid["tool_classification"]["codex"]["request_permissions"]["question_mode"] =
            Value::from("blocking");
        assert!(parse_manifest(&invalid.to_string()).is_err());
    }

    #[test]
    fn manifest_names_refuse_c1_controls() {
        let baseline: Value = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
        let mut hook = baseline.clone();
        hook["native_hooks"]["claude"]["Stop"]["native_event"] = Value::from("Stop\u{9b}");
        assert!(parse_manifest(&hook.to_string()).is_err());
        let mut tool = baseline;
        let class = tool["tool_classification"]["codex"]["request_permissions"].clone();
        tool["tool_classification"]["codex"]["request\u{85}permissions"] = class;
        assert!(parse_manifest(&tool.to_string()).is_err());
        assert!(free_of_control("caf\u{e9}\u{a0}"));
    }

    #[test]
    fn printable_json_escapes_c1_and_keeps_the_value() {
        let value = serde_json::json!({"cwd": "/tmp/a\u{9b}2J\u{85}b\u{a0}c", "n": 1});
        let text = printable_json(&value).unwrap();
        assert!(!text.chars().any(char::is_control));
        assert!(text.contains("\\u009b2J\\u0085b"));
        assert!(text.contains('\u{a0}'));
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
        let plain = serde_json::json!({"cwd": "/tmp/plain"});
        assert_eq!(printable_json(&plain).unwrap(), plain.to_string());
    }
}
