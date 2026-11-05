//! u128 as a JSON string: JavaScript numbers lose precision past 2^53.

use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
    String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
}
