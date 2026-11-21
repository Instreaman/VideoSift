use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

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

fn finite_numbers(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::Array(items) => items.iter().all(finite_numbers),
        Value::Object(fields) => fields.values().all(finite_numbers),
        _ => true,
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
        let params: Value = serde_json::from_str(inference_params)
            .map_err(|_| StorageError::CacheIdentity("invalid JSON parameters"))?;
        if !params.is_object() {
            return Err(StorageError::CacheIdentity("expected a JSON object"));
        }
        if !finite_numbers(&params) {
            return Err(StorageError::CacheIdentity(
                "non-finite inference parameter",
            ));
        }
        let mut value = serde_json::to_value(self)
            .map_err(|_| StorageError::CacheIdentity("invalid stage identity"))?;
        value["inference_params"] = params;
        value.sort_all_objects();
        serde_json::to_vec(&value)
            .map_err(|_| StorageError::CacheIdentity("invalid stage identity"))
    }
}
