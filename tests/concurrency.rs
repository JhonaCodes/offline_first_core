//! Shared access to one database from several threads, and isolation between
//! independent databases.

mod common;

use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{create_test_model, FfiDb, TestDir};
use serde_json::json;

#[test]
fn test_concurrent_reads() {
    let dir = TestDir::new("concurrent_reads");
    let state = Arc::new(dir.open("db"));

    for i in 1..=10 {
        state
            .push(create_test_model(&format!("concurrent_{i}"), None))
            .expect("push must succeed");
    }

    let handles: Vec<_> = (0..5)
        .map(|thread_id| {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                for i in 1..=10 {
                    let id = format!("concurrent_{i}");
                    let result = state.get_by_id(&id);
                    assert!(
                        result.is_ok(),
                        "Thread {thread_id} failed to read record {i}"
                    );
                    if let Ok(Some(model)) = result {
                        assert_eq!(model.id, id);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("reader thread panicked");
    }
}

#[test]
fn test_concurrent_read_during_write() {
    let dir = TestDir::new("concurrent_rw");
    let state = Arc::new(dir.open("db"));

    for i in 1..=5 {
        state
            .push(create_test_model(&format!("initial_{i}"), None))
            .expect("push must succeed");
    }

    let state_reader = Arc::clone(&state);
    let reader_handle = thread::spawn(move || {
        for _ in 0..20 {
            assert!(state_reader.get_by_id("initial_1").is_ok(), "Reader failed");
            thread::sleep(Duration::from_millis(10));
        }
    });

    let state_writer = Arc::clone(&state);
    let writer_handle = thread::spawn(move || {
        for i in 6..=15 {
            let result =
                state_writer.push(create_test_model(&format!("concurrent_write_{i}"), None));
            assert!(result.is_ok(), "Writer failed for record {i}");
            thread::sleep(Duration::from_millis(15));
        }
    });

    reader_handle.join().expect("reader thread panicked");
    writer_handle.join().expect("writer thread panicked");

    // 5 initial records + 10 written concurrently; the database is private to
    // this test, so the count is exact.
    let all_records = state.get().expect("get must succeed");
    assert_eq!(all_records.len(), 15, "Expected exactly 15 records");
}

#[test]
fn test_multiple_database_instances() {
    let dir = TestDir::new("multi_db");
    let db1 = dir.open("db_1");
    let db2 = dir.open("db_2");
    let db3 = dir.open("db_3");

    for i in 1..=3 {
        assert!(db1
            .push(create_test_model(
                &format!("db1_record_{i}"),
                Some(json!({"db": 1, "id": i}))
            ))
            .is_ok());
        assert!(db2
            .push(create_test_model(
                &format!("db2_record_{i}"),
                Some(json!({"db": 2, "id": i}))
            ))
            .is_ok());
        assert!(db3
            .push(create_test_model(
                &format!("db3_record_{i}"),
                Some(json!({"db": 3, "id": i}))
            ))
            .is_ok());
    }

    // Each database only holds its own records
    assert_eq!(db1.get().expect("get must succeed").len(), 3);
    assert_eq!(db2.get().expect("get must succeed").len(), 3);
    assert_eq!(db3.get().expect("get must succeed").len(), 3);

    assert!(db1
        .get_by_id("db2_record_1")
        .expect("lookup must succeed")
        .is_none());
    assert!(db2
        .get_by_id("db3_record_1")
        .expect("lookup must succeed")
        .is_none());
    assert!(db3
        .get_by_id("db1_record_1")
        .expect("lookup must succeed")
        .is_none());
}

/// Several threads, each with its own FFI handle to the same path, read and
/// write while another thread resets the shared database onto a new path.
///
/// The scenario runs on its own thread with a deadline, so a deadlock fails
/// the test instead of hanging it.
#[test]
fn test_ffi_concurrent_handles_with_reset() {
    let (sender, receiver) = mpsc::channel();
    let scenario = thread::spawn(move || {
        concurrent_handles_with_reset();
        // The receiver only disappears if the test already failed.
        let _ = sender.send(());
    });
    match receiver.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => scenario.join().expect("the scenario already finished"),
        // The scenario panicked before reporting: surface its own failure.
        Err(RecvTimeoutError::Disconnected) => match scenario.join() {
            Ok(()) => panic!("the scenario ended without reporting"),
            Err(payload) => panic::resume_unwind(payload),
        },
        Err(RecvTimeoutError::Timeout) => {
            panic!("the scenario did not finish within 60 s (deadlock?)")
        }
    }
}

/// The overlap is guaranteed by construction: the reset starts only after
/// `OPS_BEFORE_RESET` operations, and every worker keeps going until it has
/// completed `OPS_AFTER_RESET` operations after seeing the reset finish.
fn concurrent_handles_with_reset() {
    const WORKERS: usize = 4;
    const OPS_BEFORE_RESET: usize = 40;
    const OPS_AFTER_RESET: usize = 10;
    /// Per-worker cap, so a reset that never happens fails instead of looping.
    const MAX_OPS: usize = 5_000;

    let dir = TestDir::new("ffi_concurrent_reset");
    let main = FfiDb::open(&dir, "old");
    assert_eq!(main.push("seed").variant, "Ok");
    let new_name = dir.db_name("new");

    let ops_done = AtomicUsize::new(0);
    let reset_done = AtomicBool::new(false);

    let written_after_reset: Vec<String> = thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS)
            .map(|worker| {
                let (dir, ops_done, reset_done) = (&dir, &ops_done, &reset_done);
                scope.spawn(move || {
                    let db = FfiDb::open(dir, "old");
                    let mut after_reset = Vec::new();
                    for op in 0..MAX_OPS {
                        let saw_reset = reset_done.load(Ordering::SeqCst);
                        if saw_reset && after_reset.len() >= OPS_AFTER_RESET {
                            return after_reset;
                        }
                        let id = format!("w{worker}_{op}");
                        let pushed = db.push(&id);
                        assert_eq!(pushed.variant, "Ok", "push of `{id}`: {pushed:?}");
                        let read = db.get(&id);
                        match read.variant.as_str() {
                            "Ok" => assert_eq!(read.model().id, id),
                            // Only possible when the reset ran between both calls
                            "NotFound" => assert!(!saw_reset, "`{id}` lost after the reset"),
                            other => panic!("unexpected `{other}` reading `{id}`: {read:?}"),
                        }
                        if saw_reset {
                            after_reset.push(id);
                        }
                        ops_done.fetch_add(1, Ordering::SeqCst);
                    }
                    panic!("worker {worker} never observed the reset");
                })
            })
            .collect();

        let (dir, ops_done, reset_done, new_name) = (&dir, &ops_done, &reset_done, &new_name);
        let resetter = scope.spawn(move || {
            let db = FfiDb::open(dir, "old");
            let deadline = Instant::now() + Duration::from_secs(30);
            while ops_done.load(Ordering::SeqCst) < OPS_BEFORE_RESET {
                assert!(Instant::now() < deadline, "workers made no progress");
                thread::sleep(Duration::from_millis(1));
            }
            let response = db.reset(new_name);
            assert_eq!(response.variant, "Ok", "reset failed: {response:?}");
            reset_done.store(true, Ordering::SeqCst);
        });

        resetter.join().expect("reset thread panicked");
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("worker thread panicked"))
            .collect()
    });

    // The reset wiped the seed; every write made after it is still there.
    let ids = main.ids();
    println!(
        "{} records after the reset, {} of them written after it was seen",
        ids.len(),
        written_after_reset.len()
    );
    assert!(
        !ids.contains(&"seed".to_string()),
        "the reset must remove the seed"
    );
    for id in &written_after_reset {
        assert!(
            ids.contains(id),
            "`{id}` written after the reset is missing"
        );
    }
    // Every handle, old or new, sees the same database under the new path.
    let reopened = FfiDb::open(&dir, "new");
    assert_eq!(reopened.ids(), ids);
    let fresh = FfiDb::open(&dir, "old");
    assert!(
        fresh.ids().is_empty(),
        "the old path must open a new database"
    );
}
