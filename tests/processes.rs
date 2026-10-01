//! Real processes on one database: a writer killed mid-write, two writers
//! at once, and a full file system; with a synchronized table, every row
//! keeps its pending change through all of them (RFC-001 §19.4: a crash
//! between the row and its outbox leaves both or neither).
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
/// Set for a child that writes to a synchronized table.
const CHILD_SYNC: &str = "OFC_CHILD_SYNC";
const REMOTE: &str = "primary";
const BATCH: u64 = 10;

/// The rows table: `batch` groups the rows one transaction writes. In a
/// child started with [`CHILD_SYNC`] it is synchronized.
fn rows_table() -> TableDef {
    let table = TableDef::new("rows", "id").index("by_batch", &["batch"]);
    match env::var_os(CHILD_SYNC) {
        Some(_) => table.sync_with(REMOTE),
        None => table,
    }
}

/// Starts this test binary running only the test `name`, on `dir`.
fn spawn_child(name: &str, dir: &Path, sync: bool) -> Child {
    let mut command = Command::new(env::current_exe().expect("the test binary has a path"));
    command
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if sync {
        command.env(CHILD_SYNC, "1");
    }
    command.spawn().expect("failed to start the child process")
}

/// With a synchronized table: one pending change per row, no more, no less.
fn assert_every_row_has_its_change(db: &Db, rows: u64, context: &str) {
    let status = db.sync().status(REMOTE).expect("status");
    assert_eq!(status.pending, rows, "{context}: one change per row");
    let pending = db.sync().pending(REMOTE, None, None).expect("pending");
    assert_eq!(
        pending.changes.len() as u64,
        rows,
        "{context}: listed changes"
    );
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
    killed_writer_rounds("killed_writer", false);
}

#[test]
fn a_process_killed_mid_write_keeps_each_row_with_its_change() {
    killed_writer_rounds("killed_sync_writer", true);
}

fn killed_writer_rounds(label: &str, sync: bool) {
    let dir = TestDir::new(label);
    let path = dir.db_dir("db");

    // Three rounds: each new writer resumes after the crash of the previous
    // one, so a lock left by a killed process must not stop it.
    for round in 0..3 {
        let mut child = spawn_child("child_writes_batches_until_killed", &path, sync);
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
        if sync {
            assert_every_row_has_its_change(&db, rows.len() as u64, &format!("round {round}"));
        }
    }
}

#[test]
fn two_processes_write_the_same_database_at_once() {
    let dir = TestDir::new("two_writers");
    let path = dir.db_dir("db");
    let db = Db::open(&path).expect("open");
    db.define_table(rows_table()).expect("define");

    let mut child = spawn_child("child_writes_its_own_rows", &path, false);
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
    fill_the_file("file_full", false);
}

/// §19.4 "quota or full disk": no change committed before the disk filled
/// is lost.
#[cfg(unix)]
#[test]
fn a_full_file_system_keeps_every_committed_change() {
    fill_the_file("file_full_sync", true);
}

#[cfg(unix)]
fn fill_the_file(label: &str, sync: bool) {
    let dir = TestDir::new(label);
    let path = dir.db_dir("db");
    let binary = env::current_exe().expect("the test binary has a path");

    // 2048 blocks: 1 or 2 MiB for the data file, depending on the shell's
    // block size. libtest prints the test name before the child's lines,
    // hence `split_once` below. On macOS the child reports
    // `Storage(Other(27))`: EFBIG, as LMDB sees it.
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(r#"trap '' XFSZ; ulimit -f 2048; exec "$0" child_writes_until_the_file_is_full --exact --nocapture --test-threads=1"#)
        .arg(&binary)
        .env(CHILD_DIR, &path);
    if sync {
        command.env(CHILD_SYNC, "1");
    }
    let output = command.output().expect("failed to run the child");
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
    if sync {
        assert_every_row_has_its_change(&db, committed, "after the full file");
    }
    Query::insert_into("rows", [json!({"id": "after", "batch": 0})])
        .execute(&db)
        .expect("writable again with room");
}
