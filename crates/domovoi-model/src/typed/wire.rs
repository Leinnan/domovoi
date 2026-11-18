//! Reading a record's fields out of the JSON object it arrived as, for the
//! records decoded by hand: each field is moved out of the object, so its
//! text is never copied.

use serde::de::{DeserializeOwned, Error};
use serde_json::{Map, Value};

/// Moves the field `key` out of `fields` and decodes it; the field must be there.
pub(crate) fn required<T: DeserializeOwned, E: Error>(fields: &mut Map<String, Value>, key: &'static str) -> Result<T, E> {
    let value = fields.remove(key).ok_or_else(|| E::missing_field(key))?;
    T::deserialize(value).map_err(|error| E::custom(format_args!("`{key}`: {error}")))
}

/// Moves the field `key` out of `fields` and decodes it; absent and `null` are `None`.
pub(crate) fn optional<T: DeserializeOwned, E: Error>(fields: &mut Map<String, Value>, key: &'static str) -> Result<Option<T>, E> {
    match fields.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => T::deserialize(value).map(Some).map_err(|error| E::custom(format_args!("`{key}`: {error}"))),
    }
}

/// Moves the field `key` out of `fields` and decodes it, keeping an absent
/// field (`None`) apart from a `null` one (`Some(None)`).
pub(crate) fn nullable<T: DeserializeOwned, E: Error>(
    fields: &mut Map<String, Value>,
    key: &'static str,
) -> Result<Option<Option<T>>, E> {
    match fields.remove(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => T::deserialize(value).map(|value| Some(Some(value))).map_err(|error| E::custom(format_args!("`{key}`: {error}"))),
    }
}
