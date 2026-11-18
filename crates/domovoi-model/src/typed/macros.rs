//! Declarative macros shared by the record modules.

/// Defines a closed set of wire strings as an enum that still accepts a value
/// it does not know: `Other` keeps it verbatim, so it re-encodes unchanged.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident = $literal:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)+
            /// A value this build does not know, kept as it was sent.
            Other(Box<str>),
        }

        impl $name {
            /// The wire spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $literal,)+
                    Self::Other(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            /// The value with this wire spelling: one of the known ones, or
            /// `Other` keeping it.
            fn from(value: &str) -> Self {
                match value {
                    $($literal => Self::$variant,)+
                    other => Self::Other(other.into()),
                }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;

                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a string")
                    }

                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<$name, E> {
                        Ok($name::from(value))
                    }
                }

                deserializer.deserialize_str(Visitor)
            }
        }
    };
}

/// Implements `Deserialize` for an enum whose variant is named by a string
/// field of the record. A known tag must decode as its variant in full (a
/// malformed record is an error, never a silent fallback); an unknown tag goes
/// to the fallback variant, which keeps the record whole. The record is read
/// into a JSON map once, because the tag may follow the fields it selects.
macro_rules! tagged_union_deserialize {
    ($name:ident by $field:literal { $($tag:literal => $variant:ident,)+ _ => $fallback:ident $(,)? }) => {
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use serde::de::Error;

                let fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
                let Some(tag) = fields.get($field) else {
                    return Err(D::Error::missing_field($field));
                };
                let Some(tag) = tag.as_str() else {
                    return Err(D::Error::custom(concat!("`", $field, "` is not a string")));
                };
                match tag {
                    $($tag => serde::Deserialize::deserialize(serde_json::Value::Object(fields))
                        .map(Self::$variant)
                        .map_err(|error| D::Error::custom(format_args!(concat!(stringify!($name), " `", $tag, "`: {}"), error))),)+
                    _ => serde::Deserialize::deserialize(serde_json::Value::Object(fields))
                        .map(Self::$fallback)
                        .map_err(D::Error::custom),
                }
            }
        }
    };
}

pub(crate) use string_enum;
pub(crate) use tagged_union_deserialize;
