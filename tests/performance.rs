//! Larger datasets, repeated operation cycles and on-disk growth.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{create_test_model, unix_timestamp_secs, TestDir};
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
fn test_repeated_operations_memory_stability() {
    let dir = TestDir::new("memory_stability");
    let state = dir.open("db");

    for cycle in 0..10 {
        for i in 0..50 {
            let _ = state.push(create_test_model(
                &format!("cycle_{cycle}_record_{i}"),
                None,
            ));
        }

        for i in 0..50 {
            let _ = state.get_by_id(&format!("cycle_{cycle}_record_{i}"));
        }

        for i in 0..25 {
            let mut model = create_test_model(&format!("cycle_{cycle}_record_{i}"), None);
            model.data = json!({"updated": true, "cycle": cycle});
            let _ = state.update(model);
        }

        for i in 25..50 {
            let _ = state.delete_by_id(&format!("cycle_{cycle}_record_{i}"));
        }
    }
}

#[test]
fn test_rapid_insert_delete_cycles() {
    let dir = TestDir::new("rapid_cycles");
    let state = dir.open("db");
    let start = Instant::now();

    for cycle in 0..100 {
        for i in 0..10 {
            if state
                .push(create_test_model(&format!("stress_{cycle}_{i}"), None))
                .is_err()
            {
                info!("Insert failed at cycle {cycle} item {i}");
            }
        }

        for i in 0..10 {
            if state.delete_by_id(&format!("stress_{cycle}_{i}")).is_err() {
                info!("Delete failed at cycle {cycle} item {i}");
            }
        }

        if cycle % 20 == 0 {
            let records = state.get().unwrap_or_default();
            info!("After {cycle} cycles: {} records remaining", records.len());
        }
    }

    info!("Rapid cycles completed in {:?}", start.elapsed());
    let final_records = state.get().unwrap_or_default();
    info!("Final record count: {}", final_records.len());
}

#[test]
fn test_bulk_operations_performance() {
    let dir = TestDir::new("bulk_perf");
    let state = dir.open("db");

    // Bulk insert
    let start = Instant::now();
    for i in 0..1000 {
        let model = create_test_model(
            &format!("bulk_{i}"),
            Some(json!({
                "index": i,
                "data": format!("bulk_data_{i}"),
                "timestamp": unix_timestamp_secs()
            })),
        );
        if state.push(model).is_err() {
            info!("Bulk insert failed at record {i}");
            break;
        }
        if i % 100 == 0 {
            info!("Inserted {i} records in {:?}", start.elapsed());
        }
    }
    info!("Bulk insert completed in {:?}", start.elapsed());

    // Bulk read
    let read_start = Instant::now();
    let all_records = state.get().unwrap_or_default();
    info!(
        "Bulk read of {} records completed in {:?}",
        all_records.len(),
        read_start.elapsed()
    );

    // Random access: every 13th record
    let random_start = Instant::now();
    for i in (0..all_records.len()).step_by(13) {
        let _ = state.get_by_id(&format!("bulk_{i}"));
    }
    info!(
        "Random access test completed in {:?}",
        random_start.elapsed()
    );

    // Bulk update: every 10th record
    let update_start = Instant::now();
    for i in (0..all_records.len()).step_by(10) {
        let mut model = create_test_model(&format!("bulk_{i}"), None);
        model.data = json!({"updated": true, "original_index": i});
        let _ = state.update(model);
    }
    info!("Bulk update test completed in {:?}", update_start.elapsed());
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
