//! Binary wire types used at the Rust/Go boundary.
//!
//! These types are serialized with MessagePack on the hot path so the Go
//! wrapper can avoid JSON decoding for every start/resume/complete step. Error
//! summaries remain JSON because they are not performance-sensitive and are
//! exposed for formatting/debugging.

use std::{collections::BTreeMap, time::Duration};

use monty_types::{
    DEFAULT_MAX_RECURSION_DEPTH, ExcType, MontyDate, MontyDateTime, MontyException, MontyObject,
    MontyTimeDelta, MontyTimeZone, MontyUuid, ObjectRef, ResourceLimits, StackFrame,
    unstable::{self, MontyNode},
};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

/// Current schema version for the Go FFI.
pub const WIRE_VERSION: u32 = 2;

pub const WIRE_VALUE_NONE: u8 = 0;
pub const WIRE_VALUE_ELLIPSIS: u8 = 1;
pub const WIRE_VALUE_BOOL: u8 = 2;
pub const WIRE_VALUE_INT: u8 = 3;
pub const WIRE_VALUE_BIG_INT: u8 = 4;
pub const WIRE_VALUE_FLOAT: u8 = 5;
pub const WIRE_VALUE_STRING: u8 = 6;
pub const WIRE_VALUE_BYTES: u8 = 7;
pub const WIRE_VALUE_LIST: u8 = 8;
pub const WIRE_VALUE_TUPLE: u8 = 9;
pub const WIRE_VALUE_NAMED_TUPLE: u8 = 10;
pub const WIRE_VALUE_DICT: u8 = 11;
pub const WIRE_VALUE_SET: u8 = 12;
pub const WIRE_VALUE_FROZEN_SET: u8 = 13;
pub const WIRE_VALUE_EXCEPTION: u8 = 14;
pub const WIRE_VALUE_PATH: u8 = 15;
pub const WIRE_VALUE_DATACLASS: u8 = 16;
pub const WIRE_VALUE_FUNCTION: u8 = 17;
pub const WIRE_VALUE_REPR: u8 = 18;
pub const WIRE_VALUE_CYCLE: u8 = 19;
pub const WIRE_VALUE_DATE: u8 = 20;
pub const WIRE_VALUE_DATETIME: u8 = 21;
pub const WIRE_VALUE_TIMEDELTA: u8 = 22;
pub const WIRE_VALUE_TIMEZONE: u8 = 23;

pub const WIRE_CALL_RESULT_RETURN: u8 = 0;
pub const WIRE_CALL_RESULT_EXCEPTION: u8 = 1;
pub const WIRE_CALL_RESULT_PENDING: u8 = 2;

pub const WIRE_LOOKUP_RESULT_VALUE: u8 = 0;
pub const WIRE_LOOKUP_RESULT_UNDEFINED: u8 = 1;

pub const WIRE_PROGRESS_FUNCTION_CALL: u8 = 0;
pub const WIRE_PROGRESS_NAME_LOOKUP: u8 = 1;
pub const WIRE_PROGRESS_FUTURE: u8 = 2;
pub const WIRE_PROGRESS_COMPLETE: u8 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WirePair {
    pub key: WireValue,
    pub value: WireValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WireValue {
    pub kind: u8,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bool: bool,
    #[serde(default, skip_serializing_if = "is_zero_i64", rename = "int")]
    pub int_value: i64,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "big_int")]
    pub big_int: String,
    #[serde(default, skip_serializing_if = "is_zero_f64", rename = "float")]
    pub float_value: f64,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "string")]
    pub string_value: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
    pub bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<WireValue>,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        rename = "type_name"
    )]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "field_names")]
    pub field_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<WireValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<WirePair>,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "exc_type")]
    pub exc_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<WirePair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub year: i32,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub month: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub day: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub hour: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub minute: u8,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub second: u8,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub microsecond: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_seconds: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub days: i32,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub seconds: i32,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub microseconds: i32,
    /// Class uuid of a dataclass (class instance) value.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
    pub class_id: Vec<u8>,
    /// Instance uuid of a dataclass (class instance) value.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
    pub instance_id: Vec<u8>,
    /// Set for instances of a class defined by sandbox code rather than the host.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sandbox_class: bool,
    /// Set for instances of a class that is not a dataclass.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plain_class: bool,
}

impl WireValue {
    #[must_use]
    pub fn from_monty(obj: &MontyObject) -> Self {
        Self::from_ref(obj.as_ref())
    }

    #[must_use]
    pub fn from_ref(value: ObjectRef<'_>) -> Self {
        let children = |ids: &[unstable::NodeId]| -> Vec<Self> {
            ids.iter()
                .map(|id| Self::from_ref(unstable::child(value, *id)))
                .collect()
        };
        let pairs = |ids: &[(unstable::NodeId, unstable::NodeId)]| -> Vec<WirePair> {
            ids.iter()
                .map(|(key, item)| WirePair {
                    key: Self::from_ref(unstable::child(value, *key)),
                    value: Self::from_ref(unstable::child(value, *item)),
                })
                .collect()
        };
        match unstable::node(value) {
            MontyNode::None => Self::of(WIRE_VALUE_NONE),
            MontyNode::Ellipsis => Self::of(WIRE_VALUE_ELLIPSIS),
            MontyNode::Bool(value) => Self {
                bool: *value,
                ..Self::of(WIRE_VALUE_BOOL)
            },
            MontyNode::Int(value) => Self {
                int_value: *value,
                ..Self::of(WIRE_VALUE_INT)
            },
            MontyNode::BigInt(value) => Self {
                big_int: value.to_string(),
                ..Self::of(WIRE_VALUE_BIG_INT)
            },
            MontyNode::Float(value) => Self {
                float_value: *value,
                ..Self::of(WIRE_VALUE_FLOAT)
            },
            MontyNode::String(value) => Self {
                string_value: value.clone(),
                ..Self::of(WIRE_VALUE_STRING)
            },
            MontyNode::Bytes(value) => Self {
                bytes: value.clone(),
                ..Self::of(WIRE_VALUE_BYTES)
            },
            MontyNode::List(ids) => Self {
                items: children(ids),
                ..Self::of(WIRE_VALUE_LIST)
            },
            MontyNode::Tuple(ids) => Self {
                items: children(ids),
                ..Self::of(WIRE_VALUE_TUPLE)
            },
            MontyNode::Set(ids) => Self {
                items: children(ids),
                ..Self::of(WIRE_VALUE_SET)
            },
            MontyNode::FrozenSet(ids) => Self {
                items: children(ids),
                ..Self::of(WIRE_VALUE_FROZEN_SET)
            },
            MontyNode::NamedTuple {
                type_name,
                field_names,
                values,
            } => Self {
                type_name: type_name.clone(),
                field_names: field_names.clone(),
                values: children(values),
                ..Self::of(WIRE_VALUE_NAMED_TUPLE)
            },
            MontyNode::Dict(ids) => Self {
                pairs: pairs(ids),
                ..Self::of(WIRE_VALUE_DICT)
            },
            MontyNode::Exception { exc_type, arg } => Self {
                exc_type: exc_type.to_string(),
                arg: arg.clone(),
                ..Self::of(WIRE_VALUE_EXCEPTION)
            },
            MontyNode::Path(value) => Self {
                string_value: value.clone(),
                ..Self::of(WIRE_VALUE_PATH)
            },
            MontyNode::Function { name, docstring } => Self {
                name: name.clone(),
                docstring: docstring.clone(),
                ..Self::of(WIRE_VALUE_FUNCTION)
            },
            MontyNode::Repr(value) => Self {
                string_value: value.clone(),
                ..Self::of(WIRE_VALUE_REPR)
            },
            MontyNode::Cycle(placeholder) => Self {
                placeholder: placeholder.clone(),
                ..Self::of(WIRE_VALUE_CYCLE)
            },
            MontyNode::Date(date) => Self {
                year: date.year,
                month: date.month,
                day: date.day,
                ..Self::of(WIRE_VALUE_DATE)
            },
            MontyNode::DateTime(datetime) => Self {
                year: datetime.year,
                month: datetime.month,
                day: datetime.day,
                hour: datetime.hour,
                minute: datetime.minute,
                second: datetime.second,
                microsecond: datetime.microsecond,
                offset_seconds: datetime.offset_seconds,
                timezone_name: datetime.timezone_name.clone(),
                ..Self::of(WIRE_VALUE_DATETIME)
            },
            MontyNode::TimeDelta(delta) => Self {
                days: delta.days,
                seconds: delta.seconds,
                microseconds: delta.microseconds,
                ..Self::of(WIRE_VALUE_TIMEDELTA)
            },
            MontyNode::TimeZone(tz) => Self {
                days: tz.offset_seconds,
                timezone_name: tz.name.clone(),
                ..Self::of(WIRE_VALUE_TIMEZONE)
            },
            MontyNode::ClassInstance {
                class_type,
                instance_id,
                attrs,
            } => {
                let MontyNode::ClassType(class) = unstable::node(unstable::child(value, *class_type))
                else {
                    return Self::repr(value);
                };
                let attrs = pairs(attrs);
                Self {
                    name: class.name.clone(),
                    field_names: attrs
                        .iter()
                        .filter(|pair| pair.key.kind == WIRE_VALUE_STRING)
                        .map(|pair| pair.key.string_value.clone())
                        .collect(),
                    attrs,
                    class_id: class.id.as_bytes().to_vec(),
                    instance_id: instance_id.as_bytes().to_vec(),
                    sandbox_class: !class.host_defined,
                    plain_class: !class.is_dataclass,
                    ..Self::of(WIRE_VALUE_DATACLASS)
                }
            }
            MontyNode::NotImplemented
            | MontyNode::Complex(_)
            | MontyNode::Time(_)
            | MontyNode::Decimal(_)
            | MontyNode::Type(_)
            | MontyNode::BuiltinFunction(_)
            | MontyNode::FileHandle(_)
            | MontyNode::ClassType(_)
            | MontyNode::Module { .. } => Self::repr(value),
        }
    }

    fn of(kind: u8) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    fn repr(value: ObjectRef<'_>) -> Self {
        Self {
            string_value: value.py_repr(),
            ..Self::of(WIRE_VALUE_REPR)
        }
    }

    pub fn into_monty(self) -> Result<MontyObject, String> {
        fn all(items: Vec<WireValue>) -> Result<Vec<MontyObject>, String> {
            items.into_iter().map(WireValue::into_monty).collect()
        }
        fn all_pairs(pairs: Vec<WirePair>) -> Result<Vec<(MontyObject, MontyObject)>, String> {
            pairs
                .into_iter()
                .map(|pair| Ok((pair.key.into_monty()?, pair.value.into_monty()?)))
                .collect()
        }

        match self.kind {
            WIRE_VALUE_NONE => Ok(MontyObject::none()),
            WIRE_VALUE_ELLIPSIS => Ok(MontyObject::ellipsis()),
            WIRE_VALUE_BOOL => Ok(MontyObject::bool(self.bool)),
            WIRE_VALUE_INT => Ok(MontyObject::int(self.int_value)),
            WIRE_VALUE_BIG_INT => self
                .big_int
                .parse::<BigInt>()
                .map(MontyObject::bigint)
                .map_err(|e| format!("invalid bigint: {e}")),
            WIRE_VALUE_FLOAT => Ok(MontyObject::float(self.float_value)),
            WIRE_VALUE_STRING => Ok(MontyObject::string(self.string_value)),
            WIRE_VALUE_BYTES => Ok(MontyObject::bytes(self.bytes)),
            WIRE_VALUE_LIST => Ok(MontyObject::list(all(self.items)?)),
            WIRE_VALUE_TUPLE => Ok(MontyObject::tuple(all(self.items)?)),
            WIRE_VALUE_NAMED_TUPLE => Ok(MontyObject::named_tuple(
                self.type_name,
                self.field_names,
                all(self.values)?,
            )),
            WIRE_VALUE_DICT => Ok(MontyObject::dict(all_pairs(self.pairs)?)),
            WIRE_VALUE_SET => Ok(MontyObject::set(all(self.items)?)),
            WIRE_VALUE_FROZEN_SET => Ok(MontyObject::frozenset(all(self.items)?)),
            WIRE_VALUE_EXCEPTION => Ok(MontyObject::exception(
                self.exc_type
                    .parse::<ExcType>()
                    .map_err(|_| format!("unknown exception type: {}", self.exc_type))?,
                self.arg,
            )),
            WIRE_VALUE_PATH => Ok(MontyObject::path(self.string_value)),
            WIRE_VALUE_DATACLASS => {
                let class_id = parse_uuid(&self.class_id, "class_id")?;
                let instance_id = parse_uuid(&self.instance_id, "instance_id")?;
                let class_type = MontyObject::class_type(
                    self.name,
                    class_id,
                    !self.sandbox_class,
                    !self.plain_class,
                    Vec::new(),
                );
                Ok(MontyObject::class_instance(
                    class_type,
                    instance_id,
                    all_pairs(self.attrs)?,
                ))
            }
            WIRE_VALUE_FUNCTION => Ok(MontyObject::function(self.name, self.docstring)),
            WIRE_VALUE_REPR => Err("repr values cannot be used as Monty inputs".to_owned()),
            WIRE_VALUE_CYCLE => Err("cycle placeholders cannot be used as Monty inputs".to_owned()),
            WIRE_VALUE_DATE => Ok(MontyObject::date(MontyDate {
                year: self.year,
                month: self.month,
                day: self.day,
            })),
            WIRE_VALUE_DATETIME => Ok(MontyObject::datetime(MontyDateTime {
                year: self.year,
                month: self.month,
                day: self.day,
                hour: self.hour,
                minute: self.minute,
                second: self.second,
                microsecond: self.microsecond,
                offset_seconds: self.offset_seconds,
                timezone_name: self.timezone_name,
            })),
            WIRE_VALUE_TIMEDELTA => Ok(MontyObject::timedelta(MontyTimeDelta {
                days: self.days,
                seconds: self.seconds,
                microseconds: self.microseconds,
            })),
            WIRE_VALUE_TIMEZONE => Ok(MontyObject::timezone(MontyTimeZone {
                offset_seconds: self.days,
                name: self.timezone_name,
            })),
            other => Err(format!("unknown wire value kind: {other}")),
        }
    }
}

fn parse_uuid(bytes: &[u8], field: &str) -> Result<MontyUuid, String> {
    MontyUuid::try_from_slice(bytes)
        .ok_or_else(|| format!("dataclass {field} must be 16 bytes, got {}", bytes.len()))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireCompileOptions {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireResourceLimits {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_memory: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gc_interval: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_recursion_depth: Option<usize>,
}

impl From<WireResourceLimits> for ResourceLimits {
    fn from(value: WireResourceLimits) -> Self {
        Self {
            max_feed_duration: value.max_duration_secs.map(Duration::from_secs_f64),
            max_memory: value.max_memory,
            gc_interval: value.gc_interval,
            max_recursion_depth: value
                .max_recursion_depth
                .unwrap_or(DEFAULT_MAX_RECURSION_DEPTH),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireStartOptions {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, WireValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WireResourceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireReplOptions {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WireResourceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireFeedOptions {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, WireValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireMountOptions {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    pub virtual_path: String,
    pub host_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_bytes_limit: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireCallResult {
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<WireValue>,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "exc_type")]
    pub exc_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireLookupResult {
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<WireValue>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireFutureResults {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub results: BTreeMap<u32, WireCallResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireProgressPayload {
    pub variant: u8,
    #[serde(default = "default_wire_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub script_name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_repl: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_os_function: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_method_call: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub function_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<WireValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kwargs: Vec<WirePair>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub call_id: u32,
    /// Receiver uuid for method calls on host or sandbox class instances.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
    pub object_id: Vec<u8>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub variable_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_call_ids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<WireValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFrame {
    pub filename: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub function_name: Option<String>,
    pub source_line: Option<String>,
}

impl From<&StackFrame> for WireFrame {
    fn from(value: &StackFrame) -> Self {
        Self {
            filename: value.filename.clone(),
            line: value.start.line,
            column: value.start.column,
            end_line: value.end.line,
            end_column: value.end.column,
            function_name: value.frame_name.clone(),
            source_line: value.preview_line.as_deref().map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireErrorSummary {
    #[serde(default = "default_wire_version")]
    pub version: u32,
    pub kind: String,
    pub type_name: String,
    pub message: String,
    #[serde(default)]
    pub traceback: Vec<WireFrame>,
}

impl WireErrorSummary {
    #[must_use]
    pub fn from_exception(error: &MontyException) -> Self {
        let kind = if error.exc_type() == ExcType::SyntaxError {
            "syntax"
        } else {
            "runtime"
        };
        Self {
            version: WIRE_VERSION,
            kind: kind.to_owned(),
            type_name: error.exc_type().to_string(),
            message: error.message().unwrap_or_default().to_owned(),
            traceback: error.traceback().iter().map(WireFrame::from).collect(),
        }
    }
}

#[must_use]
pub const fn default_wire_version() -> u32 {
    WIRE_VERSION
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

const fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

const fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

const fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use monty_types::{
        ExcType, MontyDate, MontyDateTime, MontyObject, MontyTimeDelta, MontyTimeZone, MontyUuid,
    };
    use num_bigint::BigInt;

    use super::{WIRE_VALUE_DATACLASS, WIRE_VALUE_REPR, WireValue};

    fn round_trip(original: &MontyObject) -> MontyObject {
        WireValue::from_monty(original)
            .into_monty()
            .expect("wire value should round-trip")
    }

    #[test]
    fn wire_value_round_trips_nested_dicts() {
        let original = MontyObject::dict(vec![
            (
                MontyObject::string("numbers"),
                MontyObject::list(vec![
                    MontyObject::int(1),
                    MontyObject::bigint(BigInt::from(1_u64) << 80),
                ]),
            ),
            (
                MontyObject::string("path"),
                MontyObject::path("/tmp/example.txt"),
            ),
        ]);
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_class_instances() {
        let class_type = MontyObject::class_type(
            "Config",
            MontyUuid::from_u128(7),
            true,
            true,
            Vec::<(MontyObject, MontyObject)>::new(),
        );
        let original = MontyObject::class_instance(
            class_type,
            MontyUuid::from_u128(42),
            vec![
                (MontyObject::string("enabled"), MontyObject::bool(true)),
                (MontyObject::string("path"), MontyObject::path("/config.json")),
            ],
        );

        let wire = WireValue::from_monty(&original);
        assert_eq!(wire.kind, WIRE_VALUE_DATACLASS);
        assert_eq!(wire.name, "Config");
        assert_eq!(wire.field_names, vec!["enabled", "path"]);
        assert!(!wire.sandbox_class);
        assert!(!wire.plain_class);
        assert_eq!(wire.into_monty().expect("dataclass should round-trip"), original);
    }

    #[test]
    fn wire_value_rejects_dataclass_without_ids() {
        let error = WireValue {
            kind: WIRE_VALUE_DATACLASS,
            name: "Config".to_owned(),
            ..WireValue::default()
        }
        .into_monty()
        .expect_err("dataclass without ids must be rejected");
        assert_eq!(error, "dataclass class_id must be 16 bytes, got 0");
    }

    #[test]
    fn wire_value_rejects_repr_inputs() {
        let error = WireValue {
            kind: WIRE_VALUE_REPR,
            string_value: "<object repr>".to_owned(),
            ..WireValue::default()
        }
        .into_monty()
        .expect_err("repr values must be rejected");
        assert_eq!(error, "repr values cannot be used as Monty inputs");
    }

    #[test]
    fn wire_value_reports_unsupported_outputs_as_repr() {
        let wire = WireValue::from_monty(&MontyObject::complex(1.0, 2.0));
        assert_eq!(wire.kind, WIRE_VALUE_REPR);
        assert_eq!(wire.string_value, "(1+2j)");
    }

    #[test]
    fn wire_value_round_trips_exceptions() {
        let original = MontyObject::exception(ExcType::RuntimeError, Some("boom".to_owned()));
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_date() {
        let original = MontyObject::date(MontyDate {
            year: 2026,
            month: 3,
            day: 29,
        });
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_datetime() {
        let original = MontyObject::datetime(MontyDateTime {
            year: 2026,
            month: 3,
            day: 29,
            hour: 14,
            minute: 30,
            second: 45,
            microsecond: 123_456,
            offset_seconds: Some(3600),
            timezone_name: Some("UTC+01:00".to_owned()),
        });
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_naive_datetime() {
        let original = MontyObject::datetime(MontyDateTime {
            year: 2026,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            microsecond: 0,
            offset_seconds: None,
            timezone_name: None,
        });
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_timedelta() {
        let original = MontyObject::timedelta(MontyTimeDelta {
            days: -1,
            seconds: 3600,
            microseconds: 500_000,
        });
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_timezone() {
        let original = MontyObject::timezone(MontyTimeZone {
            offset_seconds: -18000,
            name: Some("EST".to_owned()),
        });
        assert_eq!(round_trip(&original), original);
    }

    #[test]
    fn wire_value_round_trips_utc_timezone() {
        let original = MontyObject::timezone(MontyTimeZone {
            offset_seconds: 0,
            name: None,
        });
        assert_eq!(round_trip(&original), original);
    }
}
