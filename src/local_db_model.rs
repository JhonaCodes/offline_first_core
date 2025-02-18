use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
#[derive(Debug,Deserialize,Serialize, Clone)]
pub struct LocalDbModel{
    pub id: String,
    pub hash: String,
    pub data: JsonValue
}