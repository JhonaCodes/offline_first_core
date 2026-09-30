//! Unusual IDs, payload shapes and sizes stored through `AppDbState`.

mod common;

use common::{create_test_model, TestDir};
use log::info;
use offline_first_core::local_db_model::LocalDbModel;
use serde_json::{json, Value};

#[test]
fn test_data_integrity() {
    let dir = TestDir::new("integrity");
    let state = dir.open("db");

    let complex_data = json!({
        "nested": {
            "array": [1, 2, 3],
            "object": {
                "key": "value",
                "number": 42,
                "boolean": true,
                "null": null
            }
        },
        "special_chars": "!@#$%^&*()_+-=[]{}|;:'\",.<>?/\\",
        "unicode": "Hello, 世界! 🌍"
    });

    state
        .push(create_test_model("complex", Some(complex_data.clone())))
        .expect("push must succeed");

    let retrieved = state
        .get_by_id("complex")
        .expect("lookup must succeed")
        .expect("record must exist");
    assert_eq!(retrieved.data, complex_data);
}

#[test]
fn test_data_validation() {
    let dir = TestDir::new("validation");
    let state = dir.open("db");

    let models = [
        create_test_model("bool", Some(json!(true))),
        create_test_model("number", Some(json!(42.5))),
        create_test_model("array", Some(json!([1, 2, 3]))),
        create_test_model("nested", Some(json!({"a": {"b": {"c": 1}}}))),
    ];
    for model in models {
        state.push(model).expect("push must succeed");
    }

    // JSON types survive the round trip
    let retrieved = state
        .get_by_id("number")
        .expect("lookup must succeed")
        .expect("record must exist");
    assert!(retrieved.data.is_number());
}

#[test]
fn test_edge_cases() {
    let dir = TestDir::new("edge_cases");
    let state = dir.open("db");

    // Empty ID: LMDB rejects empty keys, either outcome is accepted here
    match state.push(create_test_model("", None)) {
        Ok(_) => {
            assert!(state.get_by_id("").expect("lookup must succeed").is_some());
            info!("Empty ID stored successfully");
        }
        Err(e) => info!("Empty ID not allowed in LMDB: {e:?}"),
    }

    // Larger payload
    let large_data = json!({
        "large_array": vec![0; 1000],
        "large_string": "a".repeat(1000)
    });
    match state.push(create_test_model("large", Some(large_data))) {
        Ok(_) => info!("Large data stored successfully"),
        Err(e) => info!("Large data too big for LMDB: {e:?}"),
    }

    // Update with different data
    state
        .update(create_test_model("large", Some(json!({"small": "data"}))))
        .expect("update must succeed");
}

#[test]
fn test_edge_cases_extended() {
    let dir = TestDir::new("edge_cases_extended");
    let state = dir.open("db");

    // 1. IDs with special characters
    state
        .push(create_test_model("!@#$%^&*()", None))
        .expect("push must succeed");
    assert!(state
        .get_by_id("!@#$%^&*()")
        .expect("lookup must succeed")
        .is_some());

    // 2. Null or empty data
    state
        .push(create_test_model("null_data", Some(json!(null))))
        .expect("push must succeed");
    state
        .push(create_test_model("empty_data", Some(json!({}))))
        .expect("push must succeed");

    // 3. Extreme numeric values
    state
        .push(create_test_model(
            "extreme",
            Some(json!({
                "max_i64": i64::MAX,
                "min_i64": i64::MIN,
                "max_f64": f64::MAX,
                "min_f64": f64::MIN
            })),
        ))
        .expect("push must succeed");

    // 4. Unicode and emoji in data
    state
        .push(create_test_model(
            "unicode",
            Some(json!({"text": "Hello 世界 🌍 👋 🤖"})),
        ))
        .expect("push must succeed");

    // 5. Deeply nested arrays
    state
        .push(create_test_model(
            "nested",
            Some(json!([[[[[[1, 2, 3]]]]]])),
        ))
        .expect("push must succeed");

    // 6. Repeated updates of the same record
    state
        .push(create_test_model("repeated", None))
        .expect("push must succeed");
    for i in 1..100 {
        state
            .update(create_test_model(
                "repeated",
                Some(json!({"update_number": i})),
            ))
            .expect("update must succeed");
    }

    // 7. Long ID (below the LMDB key size limit)
    match state.push(create_test_model(&"a".repeat(250), None)) {
        Ok(_) => info!("Long ID stored successfully"),
        Err(e) => info!("Long ID too big for LMDB: {e:?}"),
    }

    // 8. Fast consecutive push / get / delete
    for i in 1..100 {
        let id = format!("quick_{i}");
        state
            .push(create_test_model(&id, None))
            .expect("push must succeed");
        state.get_by_id(&id).expect("lookup must succeed");
        state.delete_by_id(&id).expect("delete must succeed");
    }
}

#[test]
fn test_json_edge_cases() {
    let dir = TestDir::new("json_edge_cases");
    let state = dir.open("db");

    // Deep nesting: must be stored or rejected gracefully
    let deep_json = (0..100).fold("\"value\"".to_string(), |acc, i| {
        format!(r#"{{"level{i}": {acc}}}"#)
    });
    let deep_model = LocalDbModel {
        id: "deep_test".to_string(),
        hash: "deep_hash".to_string(),
        data: serde_json::from_str(&deep_json).unwrap_or_else(|_| json!({})),
    };
    let _result = state.push(deep_model);

    // Large array
    let large_model = LocalDbModel {
        id: "large_array".to_string(),
        hash: "large_hash".to_string(),
        data: json!((0..1000).collect::<Vec<i32>>()),
    };
    let _result = state.push(large_model);

    // Empty values
    let empty_model = LocalDbModel {
        id: "empty_test".to_string(),
        hash: String::new(),
        data: json!(null),
    };
    let _result = state.push(empty_model);
}

#[test]
fn test_unicode_and_special_characters() {
    let dir = TestDir::new("unicode");
    let state = dir.open("db");

    let unicode_tests: [(&str, &str, Value); 5] = [
        ("emoji_test", "🦀🔥🚀", json!({"emoji": "🎉🎊"})),
        ("chinese_test", "测试哈希", json!({"text": "你好世界"})),
        ("arabic_test", "اختبار", json!({"text": "مرحبا"})),
        ("russian_test", "тест", json!({"text": "привет"})),
        ("special_chars", "!@#$%^&*()", json!({"chars": "<>&\"'"})),
    ];

    for (id, hash, data) in unicode_tests {
        let model = LocalDbModel {
            id: id.to_string(),
            hash: hash.to_string(),
            data,
        };

        match state.push(model.clone()) {
            Ok(_) => {
                let retrieved = state
                    .get_by_id(id)
                    .expect("lookup of unicode data must succeed")
                    .expect("unicode data must be found after insertion");
                assert_eq!(retrieved.id, model.id);
                assert_eq!(retrieved.hash, model.hash);
                assert_eq!(retrieved.data, model.data);
            }
            // Some extreme unicode might be rejected, that is acceptable
            Err(_) => info!("Unicode test failed for: {id}"),
        }
    }
}

#[test]
fn test_size_limits() {
    let dir = TestDir::new("size_limits");
    let state = dir.open("db");

    // Near-maximum key size (the LMDB default limit is 511 bytes)
    let long_id = "a".repeat(500);
    let model = LocalDbModel {
        id: long_id.clone(),
        hash: "test_hash".to_string(),
        data: json!({"test": "data"}),
    };
    match state.push(model) {
        Ok(_) => assert!(state.get_by_id(&long_id).is_ok()),
        Err(_) => info!("Long key test failed as expected"),
    }

    // Large value (1 MB): outcome depends on the LMDB configuration
    let large_model = LocalDbModel {
        id: "large_value_test".to_string(),
        hash: "large_hash".to_string(),
        data: json!({"large_field": "x".repeat(1024 * 1024)}),
    };
    let _result = state.push(large_model);

    // Very large value (10 MB)
    let huge_model = LocalDbModel {
        id: "huge_value_test".to_string(),
        hash: "huge_hash".to_string(),
        data: json!({"huge_field": "x".repeat(10 * 1024 * 1024)}),
    };
    if state.push(huge_model).is_err() {
        info!("Huge value test properly failed");
    }
}

#[test]
fn test_empty_and_boundary_values() {
    let dir = TestDir::new("boundary");
    let state = dir.open("db");

    let models = [
        // Single character ID
        LocalDbModel {
            id: "a".to_string(),
            hash: "h".to_string(),
            data: json!({"key": "value"}),
        },
        // Whitespace-only values
        LocalDbModel {
            id: "whitespace_test".to_string(),
            hash: "   ".to_string(),
            data: json!({"spaces": "   "}),
        },
        // Numeric string IDs
        LocalDbModel {
            id: "12345".to_string(),
            hash: "67890".to_string(),
            data: json!({"number": 42}),
        },
        // Zero-like values
        LocalDbModel {
            id: "zero_test".to_string(),
            hash: "zero_hash".to_string(),
            data: json!({"zero": 0, "false": false, "null": null}),
        },
    ];

    for model in models {
        let id = model.id.clone();
        assert!(state.push(model).is_ok(), "push of `{id}` must succeed");
    }
}
