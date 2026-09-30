//! Generates the data-compatibility fixture used by `tests/compat_v0_5_0.rs`.
//!
//! The fixture pins the on-disk layout written by offline_first_core 0.5.0
//! (storage on the `lmdb` 0.8 crate): one named database `main`, key = record
//! id as UTF-8 bytes, value = the whole record serialized as JSON.
//!
//! It must only be generated with the 0.5.0 storage layer (commit `052c36e`);
//! generating it with a later version would defeat its purpose. To regenerate:
//!
//! ```text
//! git worktree add /tmp/ofc-0.5.0 052c36e
//! cp examples/gen_fixture_v0_5_0.rs /tmp/ofc-0.5.0/examples/
//! # 0.5.0 builds no rlib, which an example needs to link against the crate:
//! sed -i.bak 's/crate-type = \["staticlib", "cdylib"\]/crate-type = ["staticlib", "cdylib", "rlib"]/' /tmp/ofc-0.5.0/Cargo.toml
//! (cd /tmp/ofc-0.5.0 && cargo run --example gen_fixture_v0_5_0 -- <repo>/tests/fixtures/v0_5_0)
//! ```
//!
//! Adding `rlib` changes only how the crate is linked, not the storage layer:
//! a fixture regenerated this way is byte-identical to the committed one.
//!
//! Output: `<dir>/compat.lmdb/{data.mdb,lock.mdb}` and `<dir>/expected.json`
//! (the exact records written, used as the test oracle). The generator refuses
//! to overwrite an existing fixture.

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use offline_first_core::local_db_model::LocalDbModel;
use offline_first_core::local_db_state::AppDbState;
use serde_json::{json, Value};

const DEFAULT_OUTPUT_DIR: &str = "tests/fixtures/v0_5_0";
const DB_NAME: &str = "compat";

fn record(id: &str, hash: &str, data: Value) -> LocalDbModel {
    LocalDbModel {
        id: id.to_string(),
        hash: hash.to_string(),
        data,
    }
}

fn records() -> Vec<LocalDbModel> {
    vec![
        record(
            "ascii_simple",
            "hash_ascii_simple",
            json!({"name": "John Doe", "email": "john@example.com", "age": 30}),
        ),
        record("ascii_empty_object", "hash_empty_object", json!({})),
        record(
            "unicode_cjk",
            "哈希_cjk",
            json!({"zh": "你好世界", "ja": "こんにちは", "ko": "안녕하세요"}),
        ),
        record(
            "unicode_emoji",
            "🦀🔥",
            json!({"emoji": "🦀🔥🚀🎉", "flag": "🇨🇴"}),
        ),
        record(
            "unicode_rtl",
            "hash_rtl",
            json!({"ar": "مرحبا", "he": "שלום"}),
        ),
        record(
            "usuario_ñandú_测试",
            "hash_é_ü",
            json!({"note": "unicode in the key itself"}),
        ),
        record(
            "nested_map",
            "hash_nested_map",
            json!({
                "user": {
                    "profile": {"name": "Jane", "avatar": null},
                    "settings": {"theme": "dark", "notifications": true, "limits": {"daily": 5}}
                }
            }),
        ),
        record("list_of_ints", "hash_list_ints", json!([1, 2, 3, 4, 5])),
        record(
            "list_mixed",
            "hash_list_mixed",
            json!([1, "two", 3.5, true, null, {"k": "v"}, [1, [2, [3]]]]),
        ),
        record(
            "list_of_maps",
            "hash_list_maps",
            json!([{"id": 1, "name": "Item 1"}, {"id": 2, "name": "Item 2"}]),
        ),
        record(
            "numbers_int",
            "hash_numbers_int",
            json!({
                "zero": 0,
                "positive": 42,
                "negative": -17,
                "i64_max": i64::MAX,
                "i64_min": i64::MIN,
                "u64_max": u64::MAX
            }),
        ),
        record(
            "numbers_double",
            "hash_numbers_double",
            json!({
                "pi": std::f64::consts::PI,
                "negative": -0.5,
                "one": 1.0,
                "tiny": 1e-300,
                "huge": f64::MAX
            }),
        ),
        record("bools", "hash_bools", json!({"yes": true, "no": false})),
        record("null_data", "hash_null", Value::Null),
        record("top_level_string", "hash_string", json!("just a string")),
        record("top_level_number", "hash_number", json!(12345.678)),
        record("top_level_bool", "hash_bool", json!(false)),
        record("empty_hash", "", json!({"k": "v"})),
        record("whitespace_hash", "   ", json!({"spaces": "   "})),
        record(
            "!@#$%^&*()_+-=[]{}|;:',.<>?/",
            "hash_special_id",
            json!({
                "quotes": "\"'",
                "backslash": "\\",
                "newline": "line1\nline2",
                "tab": "a\tb"
            }),
        ),
        record(&"k".repeat(200), "hash_long_id", json!({"long_key": true})),
        record(
            "large_value",
            "hash_large_value",
            json!({"blob": "0123456789abcdef".repeat(6400), "size": 102_400}),
        ),
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir = PathBuf::from(
        env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_OUTPUT_DIR.to_string()),
    );
    let db_dir = output_dir.join(format!("{DB_NAME}.lmdb"));
    let expected_path = output_dir.join("expected.json");
    if db_dir.exists() || expected_path.exists() {
        return Err(format!(
            "refusing to overwrite the existing fixture in {}",
            output_dir.display()
        )
        .into());
    }
    fs::create_dir_all(&output_dir)?;

    let db_name = output_dir
        .join(DB_NAME)
        .to_str()
        .ok_or("output path must be valid UTF-8")?
        .to_string();
    let records = records();
    {
        let state = AppDbState::init(db_name)?;
        for model in &records {
            state.push(model.clone()).map_err(|e| e.to_string())?;
        }
    }

    fs::write(&expected_path, serde_json::to_string_pretty(&records)?)?;
    println!(
        "Wrote {} records to {} and {}",
        records.len(),
        db_dir.display(),
        expected_path.display()
    );
    Ok(())
}
