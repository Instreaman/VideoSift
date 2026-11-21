use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde::de::{Error as _, MapAccess, Visitor};
use serde_json::{Value, value::RawValue};

use super::StorageError;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Metadata,
    Subtitle,
    Asr,
    Ocr,
    Keyframe,
    Knowledge,
    Summary,
}

/// An opaque BLAKE3 digest of a versioned stage and its direct inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CacheKey(pub(super) String);

impl CacheKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bind a downstream stage to both the upstream recipe and the bytes it consumed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArtifactIdentity {
    pub(super) key: CacheKey,
    pub(super) digest: String,
}

#[derive(Serialize)]
pub struct StageIdentity<'a> {
    pub source_id: &'a str,
    pub content_version: &'a str,
    pub content_fingerprint: &'a str,
    pub part_id: &'a str,
    pub stage: Stage,
    pub model: Option<&'a str>,
    pub model_version: Option<&'a str>,
    pub algorithm_version: &'a str,
    pub prompt_version: Option<&'a str>,
    pub schema_version: &'a str,
    pub dependencies: BTreeMap<&'a str, ArtifactIdentity>,
}

fn unique_fields(input: &str) -> Result<BTreeMap<String, &RawValue>, serde_json::Error> {
    struct Fields;

    impl<'de> Visitor<'de> for Fields {
        type Value = BTreeMap<String, &'de RawValue>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object with unique keys")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut fields = BTreeMap::new();
            while let Some((name, value)) = map.next_entry()? {
                if fields.insert(name, value).is_some() {
                    return Err(M::Error::custom("duplicate object key"));
                }
            }
            Ok(fields)
        }
    }

    let mut decoder = serde_json::Deserializer::from_str(input);
    let fields = serde::Deserializer::deserialize_map(&mut decoder, Fields)?;
    decoder.end()?;
    Ok(fields)
}

// Read objects by their JSON syntax before Value sees them. With arbitrary_precision,
// Value can otherwise interpret an object beginning with its private numeric key as a number.
fn parameter_value(input: &str, depth: usize) -> Result<Value, StorageError> {
    if depth == 128 {
        return Err(StorageError::CacheIdentity(
            "inference parameters nested too deeply",
        ));
    }
    let invalid = |_: serde_json::Error| StorageError::CacheIdentity("invalid JSON parameters");
    match input.as_bytes().first() {
        Some(b'{') => unique_fields(input)
            .map_err(|_| StorageError::CacheIdentity("invalid JSON object or duplicate key"))?
            .into_iter()
            .map(|(key, value)| Ok((key, parameter_value(value.get(), depth + 1)?)))
            .collect::<Result<_, _>>()
            .map(Value::Object),
        Some(b'[') => serde_json::from_str::<Vec<&RawValue>>(input)
            .map_err(invalid)?
            .into_iter()
            .map(|value| parameter_value(value.get(), depth + 1))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        _ => {
            let value = serde_json::from_str::<Value>(input).map_err(invalid)?;
            if let Value::Number(number) = &value
                && !number.as_f64().is_some_and(f64::is_finite)
            {
                return Err(StorageError::CacheIdentity(
                    "non-finite inference parameter",
                ));
            }
            Ok(value)
        }
    }
}

impl StageIdentity<'_> {
    /// Parse parameters directly as JSON so non-finite values cannot silently become null.
    pub fn key(&self, inference_params: &str) -> Result<CacheKey, StorageError> {
        let bytes = self.canonical(inference_params)?;
        Ok(CacheKey(blake3::hash(&bytes).to_hex().to_string()))
    }

    fn canonical(&self, inference_params: &str) -> Result<Vec<u8>, StorageError> {
        if [
            self.source_id,
            self.content_version,
            self.content_fingerprint,
            self.part_id,
            self.algorithm_version,
            self.schema_version,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
            || self.model.is_some() != self.model_version.is_some()
        {
            return Err(StorageError::CacheIdentity(
                "missing stage or content version information",
            ));
        }
        let params = inference_params.trim_matches([' ', '\t', '\r', '\n']);
        if !params.starts_with('{') {
            return Err(StorageError::CacheIdentity("expected a JSON object"));
        }
        let mut value = serde_json::to_value(self)
            .map_err(|_| StorageError::CacheIdentity("invalid stage identity"))?;
        value["inference_params"] = parameter_value(params, 0)?;
        // Keep results hashed by the ambiguous legacy parser outside this namespace.
        value["key_version"] = Value::from(2);
        value.sort_all_objects();
        serde_json::to_vec(&value)
            .map_err(|_| StorageError::CacheIdentity("invalid stage identity"))
    }
}
