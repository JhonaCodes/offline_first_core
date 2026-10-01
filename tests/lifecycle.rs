//! Database lifecycle: opening, reopening, resetting, closing and naming.

mod common;

use std::fmt::Debug;
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{create_test_model, FfiDb, TestDir};
use log::info;
use offline_first_core::engine::EngineError;
use offline_first_core::local_db_state::{AppDbState, DbError};
use serde_json::json;

/// Opening may legitimately fail for unusual names; when it succeeds, the
/// database must be usable.
fn assert_usable_if_opened<E: Debug>(label: &str, result: Result<AppDbState, E>) {
    match result {
        Ok(state) => {
            state
                .push(create_test_model("probe", None))
                .unwrap_or_else(|e| panic!("{label}: opened but push failed: {e:?}"));
            let probe = state
                .get_by_id("probe")
                .unwrap_or_else(|e| panic!("{label}: opened but lookup failed: {e:?}"));
            assert!(probe.is_some(), "{label}: pushed record must be readable");
        }
        Err(e) => info!("{label}: database creation failed: {e:?}"),
    }
}

/// Requires an `Ok` response carrying the record `id`.
fn assert_found(db: &FfiDb, id: &str) {
    let response = db.get(id);
    assert_eq!(
        response.variant, "Ok",
        "`{id}` must be readable: {response:?}"
    );
    assert_eq!(response.model().id, id);
}

/// Requires an `Ok` response to a push of `id`.
fn assert_pushed(db: &FfiDb, id: &str) {
    let response = db.push(id);
    assert_eq!(
        response.variant, "Ok",
        "push of `{id}` failed: {response:?}"
    );
}

/// A second `create_db` on a path that is already open in the process returns
/// its own handle to the same database (the 0.5.0 behavior).
#[test]
fn test_db_already_open() {
    let dir = TestDir::new("already_open");
    let first = FfiDb::open(&dir, "db");

    let second = FfiDb::open(&dir, "db");

    assert_ne!(
        first.ptr(),
        second.ptr(),
        "each create_db call must return its own handle"
    );
    assert_pushed(&first, "from_first");
    assert_pushed(&second, "from_second");
    assert_found(&second, "from_first");
    assert_found(&first, "from_second");

    // Closing one handle leaves the database open for the other one
    assert_eq!(first.close().variant, "Ok");
    assert_found(&second, "from_first");
}

#[test]
fn test_ffi_close_releases_environment() {
    let dir = TestDir::new("close_releases");
    let db = FfiDb::open(&dir, "db");
    assert_pushed(&db, "persisted");

    assert_eq!(db.close().variant, "Ok");

    // Opening through the Rust API bypasses the FFI layer: LMDB only lets it
    // open the path once the environment of the closed handle is released.
    let state = AppDbState::init(dir.db_name("db"))
        .expect("close_database must release the LMDB environment");
    assert!(state
        .get_by_id("persisted")
        .expect("lookup must succeed")
        .is_some());
}

#[test]
fn test_ffi_environment_closes_with_last_handle() {
    let dir = TestDir::new("close_last_handle");
    let first = FfiDb::open(&dir, "db");
    let second = FfiDb::open(&dir, "db");

    assert_eq!(first.close().variant, "Ok");
    let still_open = AppDbState::init(dir.db_name("db"));
    assert!(
        matches!(
            still_open,
            Err(DbError::Engine(EngineError::AlreadyOpen(_)))
        ),
        "the environment must stay open while a handle is alive"
    );

    assert_eq!(second.close().variant, "Ok");
    assert!(
        AppDbState::init(dir.db_name("db")).is_ok(),
        "closing the last handle must release the environment"
    );
}

#[test]
fn test_ffi_reopen_after_close() {
    let dir = TestDir::new("reopen_after_close");
    let db = FfiDb::open(&dir, "db");
    assert_pushed(&db, "persisted");
    assert_eq!(db.close().variant, "Ok");

    let reopened = FfiDb::open(&dir, "db");

    assert_found(&reopened, "persisted");
}

/// Flutter hot restart: the Dart isolate restarts, loses its pointer without
/// closing it and calls `create_db` again on the same path.
#[test]
fn test_ffi_hot_restart_without_close() {
    let dir = TestDir::new("ffi_hot_restart");
    let before = FfiDb::open(&dir, "db");
    assert_pushed(&before, "before_restart");
    let lost = before.into_raw();

    let after = FfiDb::open(&dir, "db");

    assert_found(&after, "before_restart");
    assert_pushed(&after, "after_restart");
    assert_eq!(after.ids(), ["after_restart", "before_restart"]);

    // Cleanup only: a real hot restart never gets the lost pointer back.
    drop(after);
    drop(FfiDb::adopt(lost));
}

#[test]
fn test_ffi_reset_shared_by_two_handles() {
    let dir = TestDir::new("reset_two_handles");
    let first = FfiDb::open(&dir, "old");
    let second = FfiDb::open(&dir, "old");
    assert_pushed(&first, "before_reset");

    // Reset from another thread with a deadline, so a deadlock in the reset
    // (for example waiting for an environment that is never released) fails
    // the test instead of hanging it.
    let new_name = dir.db_name("new");
    let (sender, receiver) = mpsc::channel();
    let resetter = thread::spawn(move || {
        let response = second.reset(&new_name);
        // The receiver only disappears if the test already failed.
        let _ = sender.send(response);
        second
    });
    let Ok(response) = receiver.recv_timeout(Duration::from_secs(10)) else {
        // Releasing `first` while unwinding would wait for the stuck reset,
        // turning the failure into a hang: give it up instead.
        let _ = first.into_raw();
        panic!("reset_database did not return within 10 s (deadlock?)");
    };
    let second = resetter.join().expect("reset thread panicked");
    assert_eq!(response.variant, "Ok", "reset failed: {response:?}");

    // Both handles see the same, new and empty database
    assert_eq!(first.get("before_reset").variant, "NotFound");
    assert_pushed(&first, "after_first");
    assert_pushed(&second, "after_second");
    assert_eq!(first.ids(), ["after_first", "after_second"]);
    assert_eq!(second.ids(), ["after_first", "after_second"]);
    assert!(dir.db_dir("new").is_dir());
    assert!(!dir.db_dir("old").exists());

    // The database is now registered under its new path only
    let third = FfiDb::open(&dir, "new");
    assert_eq!(third.ids(), ["after_first", "after_second"]);
    let fresh = FfiDb::open(&dir, "old");
    assert!(
        fresh.ids().is_empty(),
        "the old path must open a new database"
    );
}

#[test]
fn test_ffi_reset_onto_same_path_keeps_sharing() {
    let dir = TestDir::new("reset_same_path_shared");
    let first = FfiDb::open(&dir, "db");
    let second = FfiDb::open(&dir, "db");
    assert_pushed(&first, "before_reset");

    let response = second.reset(&dir.db_name("db"));

    assert_eq!(response.variant, "Ok", "reset failed: {response:?}");
    assert_eq!(first.get("before_reset").variant, "NotFound");
    assert_pushed(&second, "after_reset");
    assert_found(&first, "after_reset");
    let third = FfiDb::open(&dir, "db");
    assert_found(&third, "after_reset");
}

#[test]
fn test_ffi_reset_onto_path_open_elsewhere_is_rejected() {
    let dir = TestDir::new("reset_onto_open_path");
    let a = FfiDb::open(&dir, "a");
    let b = FfiDb::open(&dir, "b");
    assert_pushed(&a, "a_record");
    assert_pushed(&b, "b_record");

    let response = a.reset(&dir.db_name("b"));

    assert_eq!(response.variant, "DatabaseError", "{response:?}");
    // Neither database changed
    assert_eq!(a.ids(), ["a_record"]);
    assert_eq!(b.ids(), ["b_record"]);
    assert!(dir.db_dir("a").is_dir());
    assert!(
        response.payload.contains("already open"),
        "unexpected message: {}",
        response.payload
    );
}

#[test]
fn test_ffi_retry_after_failed_reset_never_touches_another_database() {
    let dir = TestDir::new("reset_retry_after_failure");
    let a = FfiDb::open(&dir, "p");
    assert_pushed(&a, "a_record");

    // A regular file where the target directory must go makes the reset fail.
    fs::write(dir.db_dir("blocked"), b"not a directory").expect("create blocker file");
    let failed = a.reset(&dir.db_name("blocked"));
    assert_eq!(failed.variant, "DatabaseError", "{failed:?}");

    // The failed reset released `p`, so another database now lives there.
    let b = FfiDb::open(&dir, "p");
    assert_pushed(&b, "precious");

    // Retrying on the closed handle must fail without deleting `p`.
    let retry = a.reset(&dir.db_name("r"));
    assert_eq!(retry.variant, "DatabaseError", "{retry:?}");
    assert!(
        dir.db_dir("p").is_dir(),
        "the retry deleted another database"
    );
    assert_eq!(b.ids(), ["precious"]);
}

#[test]
fn test_reset_database() {
    let dir = TestDir::new("reset");
    let mut state = dir.open("db");

    for i in 1..=3 {
        state
            .push(create_test_model(&i.to_string(), None))
            .expect("push must succeed");
    }

    let reset = state
        .reset_database(&dir.db_name("hard_reset"))
        .expect("reset must succeed");
    assert!(reset);

    assert!(state.get().expect("get must succeed").is_empty());
}

#[test]
fn test_reset_same_name_persists_new_writes() {
    let dir = TestDir::new("reset_same_name");
    {
        let mut state = dir.open("db");
        state
            .push(create_test_model("before_reset", None))
            .expect("push must succeed");

        state
            .reset_database(&dir.db_name("db"))
            .expect("reset onto the same name must succeed");
        state
            .push(create_test_model("after_reset", None))
            .expect("push after reset must succeed");
    }

    // Writes made after the reset must land in the live files, not in the
    // deleted ones, so they are visible after reopening.
    let reopened = dir.open("db");
    let ids: Vec<String> = reopened
        .get()
        .expect("get must succeed")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, ["after_reset"]);
}

#[test]
fn test_reset_failure_rejects_further_writes() {
    let dir = TestDir::new("reset_failure");
    let mut state = dir.open("db");
    state
        .push(create_test_model("before_reset", None))
        .expect("push must succeed");

    // A regular file where the new database directory should go makes the
    // reset fail after the current database has been removed.
    fs::write(dir.db_dir("blocked"), b"not a directory").expect("failed to create the blocker");
    assert!(state.reset_database(&dir.db_name("blocked")).is_err());

    // The handle must not keep serving the removed database: a write that
    // reports success here would be silently lost.
    assert!(
        state
            .push(create_test_model("after_failed_reset", None))
            .is_err(),
        "push after a failed reset must fail"
    );
    assert!(state.get().is_err(), "get after a failed reset must fail");
}

#[test]
fn test_full_workflow() {
    let dir = TestDir::new("workflow");
    let mut state = dir.open("db");

    // 1. Create and store the initial model
    state
        .push(create_test_model("1", Some(json!({"test": "data"}))))
        .expect("push must succeed");
    thread::sleep(Duration::from_millis(100));

    // 2. get_all
    let get_all_data = state.get().expect("get must succeed");
    assert!(!get_all_data.is_empty(), "Database should not be empty");
    assert_eq!(get_all_data.len(), 1, "Should have exactly one record");

    // 3. get_by_id
    let result = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("Should find record with id 1");
    assert_eq!(result.id, "1");

    // 4. Update
    let update_result = state
        .update(create_test_model(
            "1",
            Some(json!({"test": "updated_data"})),
        ))
        .expect("update must succeed");
    assert!(update_result.is_some());
    thread::sleep(Duration::from_millis(100));

    // 5. Verify the update
    let updated = state
        .get_by_id("1")
        .expect("lookup must succeed")
        .expect("updated record must exist");
    assert_eq!(updated.data, json!({"test": "updated_data"}));

    // 6. Delete
    assert!(state.delete_by_id("1").expect("delete must succeed"));
    thread::sleep(Duration::from_millis(100));
    assert!(state.get_by_id("1").expect("lookup must succeed").is_none());

    // 7. clear_all_records with several records
    for i in 1..=3 {
        let id = i.to_string();
        state
            .push(create_test_model(&id, None))
            .expect("push must succeed");
        thread::sleep(Duration::from_millis(50));
        assert!(state.get_by_id(&id).expect("lookup must succeed").is_some());
    }
    assert_eq!(state.clear_all_records().expect("clear must succeed"), 3);
    thread::sleep(Duration::from_millis(100));

    // 8. Empty after clear
    assert!(
        state.get().expect("get must succeed").is_empty(),
        "Database should be empty after clear"
    );

    // 9. Reset onto a new database
    let reset_result = state.reset_database(&dir.db_name("after_reset"));
    assert!(reset_result.is_ok());
    thread::sleep(Duration::from_millis(100));

    // 10. Empty after reset
    assert!(
        state.get().expect("get must succeed").is_empty(),
        "Database should be empty after reset"
    );
}

#[test]
fn test_hot_restart_simulation() {
    let dir = TestDir::new("hot_restart");

    // Initial app start
    {
        let mut state = dir.open("db");

        for i in 1..=5 {
            state
                .push(create_test_model(&format!("persistent_data_{i}"), None))
                .expect("push must succeed");
        }

        let all_records = state.get().expect("get must succeed");
        assert_eq!(
            all_records.len(),
            5,
            "Data should be present before closing"
        );

        // Close before the hot restart, then drop (app termination)
        let _ = state.close_database();
        thread::sleep(Duration::from_millis(500));
        drop(state);
    }

    thread::sleep(Duration::from_millis(200));

    let db_path = dir.db_dir("db");
    assert!(
        db_path.exists(),
        "Database path does not exist: {}",
        db_path.display()
    );

    // Hot restart: reopen the same database
    {
        let state = dir.open("db");

        let all_records = state.get().expect("get must succeed");
        assert_eq!(
            all_records.len(),
            5,
            "Data should persist through hot restart"
        );
        for i in 1..=5 {
            let record = state
                .get_by_id(&format!("persistent_data_{i}"))
                .expect("lookup must succeed");
            assert!(record.is_some(), "Record {i} should persist");
        }

        // More data after the restart
        for i in 6..=10 {
            state
                .push(create_test_model(&format!("post_restart_data_{i}"), None))
                .expect("push must succeed");
        }

        let final_records = state.get().expect("get must succeed");
        assert_eq!(
            final_records.len(),
            10,
            "Should have 10 records after restart"
        );
    }
}

#[test]
fn test_multiple_instance_cleanup() {
    let dir = TestDir::new("instance_cleanup");
    let db_names: Vec<String> = (0..5).map(|i| format!("db_{i}")).collect();

    let instances: Vec<AppDbState> = db_names
        .iter()
        .enumerate()
        .map(|(i, db_name)| {
            let state = dir.open(db_name);
            state
                .push(create_test_model(&format!("data_{i}"), None))
                .expect("push must succeed");
            let records = state.get().expect("get must succeed");
            assert_eq!(records.len(), 1, "Should have 1 record in instance {i}");
            state
        })
        .collect();

    for (i, instance) in instances.iter().enumerate() {
        let record = instance
            .get_by_id(&format!("data_{i}"))
            .expect("lookup must succeed");
        assert!(record.is_some());
    }

    drop(instances);
    thread::sleep(Duration::from_millis(300));

    // Instances can be recreated and the data persisted
    for (i, db_name) in db_names.iter().enumerate() {
        let state = dir.open(db_name);
        let records = state.get().expect("get must succeed");
        assert_eq!(records.len(), 1, "Data should persist for instance {i}");
    }
}

#[test]
fn test_special_filesystem_paths() {
    let dir = TestDir::new("special_paths");

    let names = [
        // Relative path component
        "./relative_path_test".to_string(),
        // Spaces
        "space test db".to_string(),
        // Unicode
        "测试数据库".to_string(),
        // Very long name
        "very_long_database_name_".repeat(10),
    ];

    for name in names {
        assert_usable_if_opened(&name, AppDbState::init(dir.db_name(&name)));
    }
}

#[test]
fn test_database_creation_invalid_names() {
    let dir = TestDir::new("invalid_names");

    let long_name = "a".repeat(256);
    let candidates: [(&str, &str); 10] = [
        ("empty", ""),
        ("slash", "/"),
        ("backslash", "\\"),
        ("con", "CON"),
        ("prn", "PRN"),
        ("aux", "AUX"),
        ("nul_device", "NUL"),
        ("long", &long_name),
        ("nul_byte", "db\0name"),
        ("control", "db\x01name"),
    ];

    for (label, candidate) in candidates {
        // Joined as text, not with `Path::join`: `join("/")` would point at the
        // file system root instead of the test directory.
        let name = format!("{}/{candidate}", dir.path().display());
        let opened = AppDbState::init(name);

        // A name no file system accepts (256 bytes plus `.lmdb`, or a NUL)
        // must fail with an error, never panic or open something else.
        if matches!(label, "long" | "nul_byte") {
            assert!(opened.is_err(), "{label}: must be rejected");
        }
        assert_usable_if_opened(label, opened);
    }
}
