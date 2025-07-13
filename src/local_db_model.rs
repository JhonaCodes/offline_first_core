use serde::{Deserialize, Serialize};

/// LocalDbModel with data stored as JSON string for clean FFI handling.
/// 
/// Dart handles all complexity - Rust just stores/retrieves strings.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LocalDbModel {
    pub id: String,
    pub hash: String,
    /// Data as JSON string - Dart encodes/decodes, Rust just stores
    pub data: String,
}