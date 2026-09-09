use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Read};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_string_bytes: usize,
}

#[derive(Debug)]
pub enum BoundedJsonError {
    TooLarge,
    TooDeep,
    Invalid,
    Io(io::Error),
}

impl PartialEq for BoundedJsonError {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::TooLarge, Self::TooLarge)
                | (Self::TooDeep, Self::TooDeep)
                | (Self::Invalid, Self::Invalid)
                | (Self::Io(_), Self::Io(_))
        )
    }
}

impl Eq for BoundedJsonError {}

impl fmt::Display for BoundedJsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "json_too_large",
            Self::TooDeep => "json_too_deep",
            Self::Invalid => "json_invalid",
            Self::Io(_) => "json_io",
        })
    }
}

impl std::error::Error for BoundedJsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TooLarge | Self::TooDeep | Self::Invalid => None,
        }
    }
}

pub fn parse_bounded_json_with_limits<R: Read>(
    reader: R,
    limits: JsonLimits,
) -> Result<Value, BoundedJsonError> {
    let read_limit = u64::try_from(limits.max_bytes)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(BoundedJsonError::TooLarge)?;
    let mut bytes = Vec::with_capacity(limits.max_bytes.min(64 * 1024));
    reader
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(BoundedJsonError::Io)?;
    if bytes.len() > limits.max_bytes {
        return Err(BoundedJsonError::TooLarge);
    }

    validate_raw_depth(&bytes, limits.max_depth)?;
    reject_duplicate_keys(&bytes)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| BoundedJsonError::Invalid)?;
    validate_value_quotas(&value, limits)?;
    Ok(value)
}

fn validate_raw_depth(bytes: &[u8], max_depth: usize) -> Result<(), BoundedJsonError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.checked_add(1).ok_or(BoundedJsonError::TooDeep)?;
                if depth > max_depth {
                    return Err(BoundedJsonError::TooDeep);
                }
            }
            b'}' | b']' => {
                if depth == 0 {
                    return Err(BoundedJsonError::Invalid);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(BoundedJsonError::Invalid);
    }
    Ok(())
}

fn reject_duplicate_keys(bytes: &[u8]) -> Result<(), BoundedJsonError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    NoDuplicateValue::deserialize(&mut deserializer).map_err(|_| BoundedJsonError::Invalid)?;
    deserializer.end().map_err(|_| BoundedJsonError::Invalid)
}

struct NoDuplicateValue;

impl<'de> Deserialize<'de> for NoDuplicateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> Visitor<'de> for NoDuplicateVisitor {
    type Value = NoDuplicateValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateValue)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<NoDuplicateValue>()?.is_some() {}
        Ok(NoDuplicateValue)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<NoDuplicateValue>()?;
        }
        Ok(NoDuplicateValue)
    }
}

fn validate_value_quotas(value: &Value, limits: JsonLimits) -> Result<(), BoundedJsonError> {
    let mut nodes = 0usize;
    let mut stack = vec![(value, 0usize)];
    while let Some((current, depth)) = stack.pop() {
        nodes = nodes.checked_add(1).ok_or(BoundedJsonError::TooLarge)?;
        if nodes > limits.max_nodes {
            return Err(BoundedJsonError::TooLarge);
        }
        if depth > limits.max_depth {
            return Err(BoundedJsonError::TooDeep);
        }
        match current {
            Value::String(value) => {
                if value.len() > limits.max_string_bytes {
                    return Err(BoundedJsonError::TooLarge);
                }
            }
            Value::Array(values) => {
                for value in values {
                    stack.push((
                        value,
                        depth.checked_add(1).ok_or(BoundedJsonError::TooDeep)?,
                    ));
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > limits.max_string_bytes {
                        return Err(BoundedJsonError::TooLarge);
                    }
                    stack.push((
                        value,
                        depth.checked_add(1).ok_or(BoundedJsonError::TooDeep)?,
                    ));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(())
}
