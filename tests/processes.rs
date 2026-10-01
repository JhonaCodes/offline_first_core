//! Real processes on one database: a writer killed mid-write, and two
//! writers at once.
//!
//! The parent test starts this same test binary again (`current_exe`) with
//! `OFC_CHILD_DIR` set, filtered to one `child_*` test; in a normal run those
//! child tests find no `OFC_CHILD_DIR` and return at once.

mod common;

use std::env;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use common::TestDir;
use offline_first_core::engine::{col, Db, Query, TableDef};
use serde_json::{json, Value};

const CHILD_DIR: &str = "OFC_CHILD_DIR";
const BATCH: u64 = 10;

/// The rows table: `batch` groups the rows one transaction writes.
fn rows_table() -> TableDef {
    TableDef::new("rows", "id").index("by_batch", &["batch"])
}

/// Starts this test binary running only the test `name`, on `dir`.
fn spawn_child(name: &str, dir: &Path) -> Child {
    Command::new(env::current_exe().expect("the test binary has a path"))
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start the child process")
}

/// Writes batches of [`BATCH`] rows, one transaction each, after the last
/// batch already stored, until it is killed.
#[test]
fn child_writes_batches_until_killed() {
    let Some(dir) = env::var_os(CHILD_DIR) else {
        return;
    };
    let db = Db::open(&dir).expect("child: open");
    db.define_table(rows_table()).expect("child: define");
    let mut batch = Query::table("rows").count(&db).expect("child: count") / BATCH;
    loop {
        db.transaction(|tx| {
            for i in 0..BATCH {
                let id = format!("{batch:08}-{i:02}");
                Query::insert_into("rows", [json!({"id": id, "batch": batch})]).execute_in(tx)?;
            }
            Ok(())
        })
        .expect("child: commit");
        batch += 1;
    }
}

/// Writes 200 rows of its own, one transaction each, then exits.
#[test]
fn child_writes_its_own_rows() {
    let Some(dir) = env::var_os(CHILD_DIR) else {
        return;
    };
    let db = Db::open(&dir).expect("child: open");
    db.define_table(rows_table()).expect("child: define");
    for i in 0..200 {
        Query::insert_into("rows", [json!({"id": format!("child-{i:03}"), "batch": 1})])
            .execute(&db)
            .expect("child: insert");
    }
}

#[test]
fn a_process_killed_mid_write_leaves_only_whole_transactions() {
    let dir = TestDir::new("killed_writer");
    let path = dir.db_dir("db");

    // Three rounds: each new writer resumes after the crash of the previous
    // one, so a lock left by a killed process must not stop it.
    for round in 0..3 {
        let mut child = spawn_child("child_writes_batches_until_killed", &path);
        thread::sleep(Duration::from_millis(1500));
        child.kill().expect("failed to kill the child");
        child.wait().expect("failed to reap the child");

        let db = Db::open(&path).expect("reopen after the crash");
        let rows: Vec<Value> = Query::table("rows")
            .order(col("id").asc())
            .load(&db)
            .expect("load after the crash");
        assert!(
            rows.len() as u64 >= BATCH * (round + 1),
            "round {round}: the writer committed nothing ({} rows)",
            rows.len()
        );

        // Every batch is complete, and batches are contiguous from 0.
        assert_eq!(
            rows.len() as u64 % BATCH,
            0,
            "round {round}: a partial batch"
        );
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(
                row["batch"],
                json!(index as u64 / BATCH),
                "round {round}: {row}"
            );
        }
    }
}

#[test]
fn two_processes_write_the_same_database_at_once() {
    let dir = TestDir::new("two_writers");
    let path = dir.db_dir("db");
    let db = Db::open(&path).expect("open");
    db.define_table(rows_table()).expect("define");

    let mut child = spawn_child("child_writes_its_own_rows", &path);
    for i in 0..200 {
        Query::insert_into(
            "rows",
            [json!({"id": format!("parent-{i:03}"), "batch": 0})],
        )
        .execute(&db)
        .expect("parent insert while the child writes");
    }
    assert!(
        child.wait().expect("child exit").success(),
        "the child failed"
    );

    // This process sees everything the other one committed.
    assert_eq!(Query::table("rows").count(&db).expect("count"), 400);
    let theirs = Query::table("rows")
        .filter(col("batch").eq(1))
        .count(&db)
        .expect("count the child's rows");
    assert_eq!(theirs, 200);
}

/// Writes rows of 4 KiB, one transaction each, until a write fails; prints
/// the number committed and the error, and exits normally.
#[test]
fn child_writes_until_the_file_is_full() {
    let Some(dir) = env::var_os(CHILD_DIR) else {
        return;
    };
    let db = Db::open(&dir).expect("child: open");
    db.define_table(rows_table()).expect("child: define");
    let filler = "x".repeat(4096);
    let mut committed = 0u64;
    let error = loop {
        let row = json!({"id": format!("{committed:08}"), "batch": committed, "filler": filler});
        match Query::insert_into("rows", [row]).execute(&db) {
            Ok(_) => committed += 1,
            Err(error) => break error,
        }
    };
    println!("COMMITTED {committed}");
    println!("ERROR {error:?}");
}

/// Disk full, simulated without privileges: the child runs under a file
/// size limit (`ulimit -f`, with SIGXFSZ ignored so the write fails with an
/// error instead of killing it), so LMDB's writes fail as on a full disk.
#[cfg(unix)]
#[test]
fn a_full_file_system_is_an_error_and_keeps_every_commit() {
    let dir = TestDir::new("file_full");
    let path = dir.db_dir("db");
    let binary = env::current_exe().expect("the test binary has a path");

    // 2048 blocks: 1 or 2 MiB for the data file, depending on the shell's
    // block size. libtest prints the test name before the child's lines,
    // hence `split_once` below. On macOS the child reports
    // `Storage(Other(27))`: EFBIG, as LMDB sees it.
    let output = Command::new("sh")
        .arg("-c")
        .arg(r#"trap '' XFSZ; ulimit -f 2048; exec "$0" child_writes_until_the_file_is_full --exact --nocapture --test-threads=1"#)
        .arg(&binary)
        .env(CHILD_DIR, &path)
        .output()
        .expect("failed to run the child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "the child crashed: {stdout}");

    let committed: u64 = stdout
        .lines()
        .find_map(|line| line.split_once("COMMITTED ").map(|(_, count)| count))
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("the child reported no count: {stdout}"));
    let error = stdout
        .lines()
        .find_map(|line| line.split_once("ERROR ").map(|(_, error)| error))
        .unwrap_or_else(|| panic!("the child reported no error: {stdout}"));
    assert!(committed > 0, "nothing fit under the limit: {stdout}");
    assert!(
        error.starts_with("Storage(") || error.starts_with("Io(") || error == "MapFull",
        "a full file is a storage error, got {error}"
    );

    // Reopened without the limit: exactly the committed rows, and writable.
    let db = Db::open(&path).expect("reopen after the full file");
    assert_eq!(Query::table("rows").count(&db).expect("count"), committed);
    Query::insert_into("rows", [json!({"id": "after", "batch": 0})])
        .execute(&db)
        .expect("writable again with room");
}
