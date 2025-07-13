use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// LocalDbModel for communication between Dart and Rust layers.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LocalDbModel {
    pub id: String,
    pub hash: Option<String>,
    /// Data as JsonValue - matches Dart's Map<String, dynamic>
    pub data: JsonValue,
}