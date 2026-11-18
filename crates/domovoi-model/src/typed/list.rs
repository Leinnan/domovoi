//! Lists a newer server may put elements in that this build cannot read.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::ops::Deref;

/// A list read the way t3code's `ForwardCompatibleArray` reads one: an
/// element that does not fit the schema this build knows is left out, and the
/// rest is read.
///
/// The elements read are the list (it derefs to a slice of them). The ones
/// left out are kept as they were sent, each with its place, so the list
/// encodes back to what it came from and a reader can tell, and say, that
/// something was left out ([`ForwardList::unread`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ForwardList<T> {
    items: Vec<T>,
    /// The elements left out, each with its index in the list as sent.
    unread: Vec<(usize, Value)>,
}

impl<T> ForwardList<T> {
    /// How many elements of the list as sent this build could not read.
    pub fn unread(&self) -> usize {
        self.unread.len()
    }

    /// The elements left out, as they were sent.
    pub fn left_out(&self) -> impl Iterator<Item = &Value> {
        self.unread.iter().map(|(_, sent)| sent)
    }

    /// The elements read, to change them. The ones left out keep their places.
    pub fn items_mut(&mut self) -> &mut Vec<T> {
        &mut self.items
    }

    /// The elements read, and the ones left out as they were sent, each in
    /// the order they came.
    pub fn into_parts(self) -> (Vec<T>, Vec<Value>) {
        (self.items, self.unread.into_iter().map(|(_, sent)| sent).collect())
    }
}

impl<T> Default for ForwardList<T> {
    fn default() -> Self {
        Self { items: Vec::new(), unread: Vec::new() }
    }
}

impl<T> From<Vec<T>> for ForwardList<T> {
    fn from(items: Vec<T>) -> Self {
        Self { items, unread: Vec::new() }
    }
}

impl<T> Deref for ForwardList<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<'a, T> IntoIterator for &'a ForwardList<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<T: Serialize> Serialize for ForwardList<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;

        let mut sequence = serializer.serialize_seq(Some(self.items.len() + self.unread.len()))?;
        let (mut items, mut unread) = (self.items.iter(), self.unread.iter().peekable());
        for index in 0..self.items.len() + self.unread.len() {
            match unread.next_if(|(at, _)| *at == index) {
                Some((_, sent)) => sequence.serialize_element(sent)?,
                None => match items.next() {
                    Some(item) => sequence.serialize_element(item)?,
                    // Items were removed since: what was left out follows them.
                    None => break,
                },
            }
        }
        for (_, sent) in unread {
            sequence.serialize_element(sent)?;
        }
        sequence.end()
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for ForwardList<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let sent = Vec::<Value>::deserialize(deserializer)?;
        let mut list = Self { items: Vec::with_capacity(sent.len()), unread: Vec::new() };
        for (index, element) in sent.into_iter().enumerate() {
            match T::deserialize(&element) {
                Ok(item) => list.items.push(item),
                Err(_) => list.unread.push((index, element)),
            }
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_element_that_does_not_fit_is_left_out_and_kept() {
        let sent = json!([1, "two", 3, { "four": 4 }]);
        let list: ForwardList<u64> = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!((&*list, list.unread()), (&[1, 3][..], 2));
        assert_eq!(list.left_out().collect::<Vec<_>>(), [&json!("two"), &json!({ "four": 4 })]);
        assert_eq!(serde_json::to_value(&list).unwrap(), sent);
    }
}
