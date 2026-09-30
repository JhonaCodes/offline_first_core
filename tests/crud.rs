//! CRUD behavior of `AppDbState` against a real LMDB environment.

mod common;

use common::{create_test_model, unix_timestamp_secs, TestDir};
use serde_json::json;

#[test]
fn test_push() {
    let dir = TestDir::new("push");
    let state = dir.open("db");
    let model = create_test_model("1", None);

    let result = state.push(model.clone()).expect("push must succeed");
    assert_eq!(result.id, model.id);
    assert_eq!(result.hash, model.hash);

    // Verify that it was saved correctly
    let stored = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("pushed record must exist");
    assert_eq!(stored.id, model.id);
}

#[test]
fn test_get_by_id() {
    let dir = TestDir::new("get_by_id");
    let state = dir.open("db");

    // Non-existent ID
    let no_result = state.get_by_id("nonexistent").expect("lookup must succeed");
    assert!(no_result.is_none());

    // Existing ID
    state
        .push(create_test_model("1", None))
        .expect("push must succeed");
    let found = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("pushed record must exist");
    assert_eq!(found.id, "1");
}

#[test]
fn test_get_all() {
    let dir = TestDir::new("get_all");
    let mut state = dir.open("db");

    // Reset onto the same name before starting
    state
        .reset_database(&dir.db_name("db"))
        .expect("reset must succeed");

    let empty_results = state.get().expect("get must succeed");
    assert!(
        empty_results.is_empty(),
        "Database should be initially empty"
    );

    for (count, id) in ["1", "2", "3"].into_iter().enumerate() {
        state
            .push(create_test_model(id, None))
            .expect("push must succeed");
        let results = state.get().expect("get must succeed");
        assert_eq!(
            results.len(),
            count + 1,
            "Should have exactly {} records",
            count + 1
        );
    }

    // Each record is reachable individually
    for id in ["1", "2", "3"] {
        assert!(state.get_by_id(id).expect("lookup must succeed").is_some());
    }
}

#[test]
fn test_update() {
    let dir = TestDir::new("update");
    let state = dir.open("db");

    // Updating a non-existent record reports None
    let update_result = state
        .update(create_test_model("999", None))
        .expect("update must succeed");
    assert!(update_result.is_none());

    // Updating an existing record
    state
        .push(create_test_model("1", Some(json!({"original": true}))))
        .expect("push must succeed");
    let updated_model = create_test_model("1", Some(json!({"updated": true})));
    let result = state
        .update(updated_model.clone())
        .expect("update must succeed");
    assert!(result.is_some());

    let updated = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("updated record must exist");
    assert_eq!(updated.data, updated_model.data);
}

#[test]
fn test_delete() {
    let dir = TestDir::new("delete");
    let state = dir.open("db");

    // Deleting a non-existent record reports false
    assert!(!state
        .delete_by_id("nonexistent")
        .expect("delete must succeed"));

    // Deleting an existing record
    state
        .push(create_test_model("1", None))
        .expect("push must succeed");
    assert!(state.delete_by_id("1").expect("delete must succeed"));
    assert!(state.get_by_id("1").expect("lookup must succeed").is_none());
}

#[test]
fn test_clear_all_records() {
    let dir = TestDir::new("clear");
    let state = dir.open("db");

    // Clearing an empty database
    assert_eq!(state.clear_all_records().expect("clear must succeed"), 0);

    // Clearing a database with records
    for i in 1..=3 {
        state
            .push(create_test_model(&i.to_string(), None))
            .expect("push must succeed");
    }
    assert_eq!(state.clear_all_records().expect("clear must succeed"), 3);
    assert!(state.get().expect("get must succeed").is_empty());
}

#[test]
fn test_basic_operations() {
    let dir = TestDir::new("basic");
    let state = dir.open("db");

    // Insert multiple records in sequence
    for i in 1..=5 {
        let result = state
            .push(create_test_model(&i.to_string(), None))
            .expect("push must succeed");
        assert_eq!(result.id, i.to_string());
    }

    let all_records = state.get().expect("get must succeed");
    assert_eq!(all_records.len(), 5, "Should have inserted 5 records");

    // Each record exists and carries the expected hash
    for i in 1..=5 {
        let record = state
            .get_by_id(&i.to_string())
            .expect("lookup must succeed");
        let record = record.unwrap_or_else(|| panic!("Record {i} should exist"));
        assert_eq!(record.hash, format!("hash_{i}"));
    }
}

#[test]
fn test_error_handling() {
    let dir = TestDir::new("error_handling");
    let state = dir.open("db");

    // Operations with non-existent IDs
    assert!(state
        .get_by_id("nonexistent")
        .expect("lookup must succeed")
        .is_none());
    assert!(!state
        .delete_by_id("nonexistent")
        .expect("delete must succeed"));
    assert!(state
        .update(create_test_model("nonexistent", None))
        .expect("update must succeed")
        .is_none());

    // Operations after clearing the database
    state
        .push(create_test_model("1", None))
        .expect("push must succeed");
    state.clear_all_records().expect("clear must succeed");
    assert!(state.get_by_id("1").expect("lookup must succeed").is_none());
}

#[test]
fn test_interrupted_operations() {
    let dir = TestDir::new("interrupted");
    let state = dir.open("db");

    state
        .push(create_test_model("1", None))
        .expect("push must succeed");

    // Update and delete the same record back to back
    state
        .update(create_test_model("1", Some(json!({"updated": true}))))
        .expect("update must succeed");
    state.delete_by_id("1").expect("delete must succeed");

    assert!(state.get_by_id("1").expect("lookup must succeed").is_none());
}

#[test]
fn test_recovery_after_errors() {
    let dir = TestDir::new("recovery");
    let state = dir.open("db");

    state
        .push(create_test_model("1", None))
        .expect("push must succeed");

    // A lookup miss is handled gracefully
    assert!(state.get_by_id("nonexistent").is_ok());

    // The database keeps operating afterwards
    assert!(state.push(create_test_model("2", None)).is_ok());
}

#[test]
fn test_batch_operations() {
    let dir = TestDir::new("batch");
    let state = dir.open("db");

    // Insert 99 records
    for i in 1..100 {
        state
            .push(create_test_model(&i.to_string(), None))
            .expect("push must succeed");
    }

    // Delete 49 of them
    for i in 1..50 {
        state
            .delete_by_id(&i.to_string())
            .expect("delete must succeed");
    }

    let remaining = state.get().expect("get must succeed");
    assert_eq!(remaining.len(), 50);
}

#[test]
fn test_data_consistency() {
    let dir = TestDir::new("consistency");
    let state = dir.open("db");

    state
        .push(create_test_model(
            "1",
            Some(json!({"count": 0, "timestamp": unix_timestamp_secs()})),
        ))
        .expect("push must succeed");

    // Several successive updates
    for i in 1..10 {
        state
            .update(create_test_model(
                "1",
                Some(json!({"count": i, "timestamp": unix_timestamp_secs()})),
            ))
            .expect("update must succeed");
    }

    let final_state = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("record must exist");
    assert_eq!(final_state.data["count"], 9);
}

#[test]
fn test_case_sensitivity() {
    let dir = TestDir::new("case");
    let state = dir.open("db");

    for id in ["lowercase_id", "UPPERCASE_ID", "MixedCase_ID"] {
        assert!(state.push(create_test_model(id, None)).is_ok());
    }

    let exists = |id: &str| state.get_by_id(id).expect("lookup must succeed").is_some();
    assert!(exists("lowercase_id"));
    assert!(!exists("LOWERCASE_ID"));
    assert!(exists("UPPERCASE_ID"));
    assert!(!exists("uppercase_id"));
    assert!(exists("MixedCase_ID"));
    assert!(!exists("mixedcase_id"));
}
