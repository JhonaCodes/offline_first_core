use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
#[derive(Deserialize,Serialize,Debug)]
pub struct LocalDbModel{
    pub id: String,
    pub hash: String,
    pub data: JsonValue
}