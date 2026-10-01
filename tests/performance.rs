//! Larger datasets, repeated operation cycles and on-disk growth.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{create_test_model, TestDir};
use log::info;
use offline_first_core::local_db_model::LocalDbModel;
use serde_json::json;

/// Total size in bytes of the files directly inside an LMDB directory.
fn database_size(db_dir: &Path) -> u64 {
    match fs::read_dir(db_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum(),
        Err(_) => 0,
    }
}

#[test]
fn test_large_dataset() {
    let dir = TestDir::new("large_dataset");
    let state = dir.open("db");

    for i in 1..=100 {
        state
            .push(create_test_model(
                &i.to_string(),
                Some(json!({
                    "name": format!("test_{i}"),
                    "value": i,
                    "data": vec![1, 2, 3, 4, 5]
                })),
            ))
            .expect("push must succeed");
    }

    assert_eq!(state.get().expect("get must succeed").len(), 100);

    // Point lookup stays fast
    let start = Instant::now();
    let _result = state.get_by_id("50").expect("lookup must succeed");
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "Lookup took too long"
    );
}

#[test]
fn test_memory_usage_large_dataset() {
    let dir = TestDir::new("memory_large_dataset");
    let state = dir.open("db");

    for i in 0..1000 {
        let model = LocalDbModel {
            id: format!("memory_test_{i}"),
            hash: format!("hash_{i}"),
            data: json!({
                "id": i,
                "data": "x".repeat(1024),
                "nested": {
                    "array": (0..100).collect::<Vec<i32>>(),
                    "string": format!("test_string_{i}")
                }
            }),
        };

        if let Err(e) = state.push(model) {
            info!("Memory test stopped at record {i} due to: {e:?}");
            break;
        }
    }

    for i in 0..10 {
        let id = format!("memory_test_{i}");
        if let Ok(Some(model)) = state.get_by_id(&id) {
            assert_eq!(model.id, id);
        }
    }
}

#[test]
fn test_repeated_cycles_leave_exactly_the_surviving_records() {
    let dir = TestDir::new("repeated_cycles");
    let state = dir.open("db");

    for cycle in 0..10 {
        for i in 0..50 {
            state
                .push(create_test_model(
                    &format!("cycle_{cycle}_record_{i:02}"),
                    None,
                ))
                .expect("push must succeed");
        }
        for i in 0..50 {
            assert!(
                state
                    .get_by_id(&format!("cycle_{cycle}_record_{i:02}"))
                    .expect("lookup must succeed")
                    .is_some(),
                "a pushed record must be readable"
            );
        }
        for i in 0..25 {
            let mut model = create_test_model(&format!("cycle_{cycle}_record_{i:02}"), None);
            model.data = json!({"updated": true, "cycle": cycle});
            assert!(state.update(model).expect("update must succeed").is_some());
        }
        for i in 25..50 {
            assert!(state
                .delete_by_id(&format!("cycle_{cycle}_record_{i:02}"))
                .expect("delete must succeed"));
        }
    }

    // 25 updated survivors per cycle, nothing else.
    let records = state.get().expect("get must succeed");
    assert_eq!(records.len(), 10 * 25);
    for record in records {
        let (cycle, index) = record
            .id
            .strip_prefix("cycle_")
            .and_then(|rest| rest.split_once("_record_"))
            .expect("only the test's records exist");
        let cycle: u64 = cycle.parse().expect("numeric cycle");
        assert!(index.parse::<u32>().expect("numeric index") < 25);
        assert_eq!(record.data, json!({"updated": true, "cycle": cycle}));
    }
}

#[test]
fn test_rapid_insert_delete_cycles() {
    let dir = TestDir::new("rapid_cycles");
    let state = dir.open("db");
    let start = Instant::now();

    for cycle in 0..100 {
        for i in 0..10 {
            state
                .push(create_test_model(&format!("stress_{cycle}_{i}"), None))
                .expect("push must succeed");
        }
        for i in 0..10 {
            assert!(
                state
                    .delete_by_id(&format!("stress_{cycle}_{i}"))
                    .expect("delete must succeed"),
                "a pushed record must exist until deleted"
            );
        }
        assert!(
            state.get().expect("get must succeed").is_empty(),
            "cycle {cycle} must leave no record behind"
        );
    }

    info!("Rapid cycles completed in {:?}", start.elapsed());
}

#[test]
fn test_bulk_insert_read_and_update_round_trip() {
    let dir = TestDir::new("bulk_round_trip");
    let state = dir.open("db");

    let start = Instant::now();
    for i in 0..1000 {
        let model = create_test_model(
            &format!("bulk_{i:04}"),
            Some(json!({"index": i, "data": format!("bulk_data_{i}")})),
        );
        state.push(model).expect("push must succeed");
    }
    info!("Bulk insert of 1000 records in {:?}", start.elapsed());

    // Everything comes back, in id order.
    let all_records = state.get().expect("get must succeed");
    let ids: Vec<String> = all_records.iter().map(|record| record.id.clone()).collect();
    let expected: Vec<String> = (0..1000).map(|i| format!("bulk_{i:04}")).collect();
    assert_eq!(ids, expected);

    // Random access reads the record that was written.
    for i in (0..1000).step_by(13) {
        let record = state
            .get_by_id(&format!("bulk_{i:04}"))
            .expect("lookup must succeed")
            .expect("the record must exist");
        assert_eq!(record.data["index"], json!(i));
    }

    // Updating every 10th record changes exactly those.
    for i in (0..1000).step_by(10) {
        let mut model = create_test_model(&format!("bulk_{i:04}"), None);
        model.data = json!({"updated": true, "original_index": i});
        assert!(state.update(model).expect("update must succeed").is_some());
    }
    for record in state.get().expect("get must succeed") {
        let index: usize = record.id["bulk_".len()..].parse().expect("numeric id");
        assert_eq!(
            record.data.get("updated").is_some(),
            index % 10 == 0,
            "{}",
            record.id
        );
    }
}

#[test]
fn test_database_size_growth() {
    let dir = TestDir::new("size_growth");
    let state = dir.open("db");
    let mut sizes = Vec::new();

    for batch in 0..10 {
        for i in 0..100 {
            let _ = state.push(create_test_model(
                &format!("size_test_{batch}_{i}"),
                Some(json!({
                    "batch": batch,
                    "index": i,
                    "payload": "x".repeat(100)
                })),
            ));
        }

        let db_size = database_size(&dir.db_dir("db"));
        sizes.push(db_size);
        info!(
            "After batch {batch}: {} records, {} KB database size",
            (batch + 1) * 100,
            db_size / 1024
        );
    }

    for pair in sizes.windows(2) {
        assert!(pair[1] >= pair[0], "Database size should not decrease");
    }
}
