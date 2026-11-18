//! Shared by the tests that read the recorded sessions.

use serde_json::{Map, Value};

/// Expands t3code's compact id table in place (`orchestrationV2IdTable`), as
/// any reader of a snapshot does: on each record directly under `container`
/// (an element of a list, or an object) and on its `item`, a number under `id`
/// or `…Id` indexes `ids`, as does each number in a `…Ids` list.
///
/// The crate expands the table itself when it decodes a snapshot. The tests
/// expand it here, on the JSON, to have what the decoded snapshot must encode to.
pub fn expand_record_ids(container: &mut Map<String, Value>, ids: &[Value]) {
    fn expand(id: &mut Value, ids: &[Value]) {
        if let Some(expanded) = id.as_u64().and_then(|index| ids.get(index as usize)) {
            *id = expanded.clone();
        }
    }
    fn expand_record(record: &mut Map<String, Value>, ids: &[Value]) {
        for (key, value) in record.iter_mut() {
            match value {
                Value::Number(_) if key == "id" || key.ends_with("Id") => expand(value, ids),
                Value::Array(list) if key.ends_with("Ids") => list.iter_mut().for_each(|id| expand(id, ids)),
                Value::Object(item) if key == "item" => expand_record(item, ids),
                _ => {}
            }
        }
    }
    for value in container.values_mut() {
        match value {
            Value::Array(records) => records.iter_mut().filter_map(Value::as_object_mut).for_each(|record| expand_record(record, ids)),
            Value::Object(record) => expand_record(record, ids),
            _ => {}
        }
    }
}
