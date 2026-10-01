//! The memory map grows ahead of the writes: transactions one after another
//! never meet a full map, and while several threads write to a synchronized
//! table (retrying a transaction that met one, as documented) and others
//! read it, no write or pending change is lost and readers never fail.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use common::TestDir;
use offline_first_core::engine::{
    col, Db, Durability, EngineError, OpenOptions, Query, TableDef, WriteTx,
};
use serde_json::json;

const WRITERS: u64 = 4;
const TRANSACTIONS: u64 = 40;
const ROWS: u64 = 25;
const INITIAL_MAP: usize = 256 << 10;

fn small_map() -> OpenOptions {
    OpenOptions {
        initial_map_size: INITIAL_MAP,
        max_map_size: 1 << 30,
        durability: Durability::NoSync,
        ..OpenOptions::default()
    }
}

/// Runs `body` in a transaction again while it meets a full map; answers
/// how many attempts met one.
fn retrying(db: &Db, mut body: impl FnMut(&mut WriteTx<'_>) -> Result<(), EngineError>) -> u64 {
    let mut full = 0;
    loop {
        match db.transaction(&mut body) {
            Ok(()) => return full,
            Err(EngineError::MapFull) => full += 1,
            Err(other) => panic!("a write failed while the map grew: {other}"),
        }
    }
}

#[test]
fn transactions_one_after_another_never_meet_a_full_map() {
    let dir = TestDir::new("stress_sequential");
    let db = Db::open_with(dir.db_dir("db"), small_map()).expect("open");
    db.define_table(TableDef::new("blobs", "id"))
        .expect("define");
    let filler = "x".repeat(2048);

    // 200 transactions of 10 rows of 2 KiB: the map doubles several times.
    for transaction in 0..200u64 {
        db.transaction(|tx| {
            let rows = (0..10u64)
                .map(|row| json!({"id": format!("{transaction}-{row}"), "filler": filler}));
            Query::insert_into("blobs", rows).execute_in(tx)?;
            Ok(())
        })
        .unwrap_or_else(|e| panic!("transaction {transaction} failed: {e}"));
    }

    assert_eq!(Query::table("blobs").count(&db).expect("count"), 2000);
    assert!(db.map_size().expect("map size") >= INITIAL_MAP * 16);
}

#[test]
fn the_map_grows_under_concurrent_writers_and_readers() {
    let dir = TestDir::new("stress_growth");
    let db = Db::open_with(dir.db_dir("db"), small_map()).expect("open");
    db.define_table(
        TableDef::new("blobs", "id")
            .index("by_writer", &["writer"])
            .sync_with("primary"),
    )
    .expect("define");
    let filler = "x".repeat(2048);
    let done = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..2)
        .map(|_| {
            let db = db.clone();
            let done = Arc::clone(&done);
            thread::spawn(move || {
                let mut last = 0;
                let mut reads = 0u64;
                while !done.load(Ordering::SeqCst) {
                    let count = Query::table("blobs")
                        .count(&db)
                        .expect("a read never fails");
                    assert!(count >= last, "a committed row disappeared");
                    last = count;
                    reads += 1;
                }
                reads
            })
        })
        .collect();
    let writers: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let db = db.clone();
            let filler = filler.clone();
            thread::spawn(move || {
                (0..TRANSACTIONS)
                    .map(|transaction| {
                        retrying(&db, |tx| {
                            let rows = (0..ROWS).map(|row| {
                                json!({
                                    "id": format!("{writer}-{transaction}-{row}"),
                                    "writer": writer,
                                    "filler": filler,
                                })
                            });
                            Query::insert_into("blobs", rows).execute_in(tx)?;
                            Ok(())
                        })
                    })
                    .sum::<u64>()
            })
        })
        .collect();

    let full: u64 = writers.into_iter().map(|w| w.join().expect("writer")).sum();
    done.store(true, Ordering::SeqCst);
    let reads: u64 = readers.into_iter().map(|r| r.join().expect("reader")).sum();

    let total = WRITERS * TRANSACTIONS * ROWS;
    assert_eq!(Query::table("blobs").count(&db).expect("count"), total);
    for writer in 0..WRITERS {
        let rows = Query::table("blobs")
            .filter(col("writer").eq(writer))
            .count(&db)
            .expect("count by writer");
        assert_eq!(rows, TRANSACTIONS * ROWS, "writer {writer}");
    }
    assert_eq!(
        db.sync().status("primary").expect("status").pending,
        total,
        "every row kept its change"
    );
    let grown = db.map_size().expect("map size");
    assert!(grown > INITIAL_MAP * 16, "the map grew: {grown} bytes");
    println!("{reads} reads while the map grew to {grown} bytes; {full} attempts met a full map");
    assert!(
        full < WRITERS * TRANSACTIONS / 4,
        "growth ahead keeps full maps rare"
    );
}
