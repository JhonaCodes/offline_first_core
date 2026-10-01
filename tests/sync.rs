//! Offline-first sync inside the transaction (RFC-001 §13): a write to a
//! synchronized table records its change in the same commit; the engine
//! knows what is pending, what is being sent and what the server confirmed,
//! per mutation and revision. The invariants I1–I12 of §13.19 name the
//! tests.

mod common;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::TestDir;
use offline_first_core::engine::sync::{
    Acknowledgement, ClaimLimits, ClaimedBatch, DeliveryKind, Envelope, Operation, PushResult,
    Rejection, RemoteChange, RemotePage, Resolution, StateKind,
};
use offline_first_core::engine::{col, Db, EngineError, Query, TableDef};
use serde_json::{json, Value};

const REMOTE: &str = "primary";

fn open(dir: &TestDir) -> Db {
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(
        TableDef::new("notes", "id")
            .index("by_title", &["title"])
            .sync_with(REMOTE),
    )
    .expect("define notes");
    db.define_table(TableDef::new("cache", "id"))
        .expect("define cache");
    db
}

fn insert(db: &Db, id: &str, title: &str) {
    Query::insert_into("notes", [json!({"id": id, "title": title})])
        .execute(db)
        .expect("insert");
}

fn retitle(db: &Db, id: &str, title: &str) {
    Query::update("notes")
        .filter(col("id").eq(id))
        .set("title", title)
        .execute(db)
        .expect("update");
}

fn note(db: &Db, id: &str) -> Option<Value> {
    db.first(&Query::table("notes").filter(col("id").eq(id)))
        .expect("find")
}

fn claim(db: &Db) -> ClaimedBatch {
    db.sync()
        .claim(REMOTE, &ClaimLimits::default())
        .expect("claim")
}

fn claim_expired(db: &Db) -> ClaimedBatch {
    let limits = ClaimLimits {
        lease_ms: 0,
        ..ClaimLimits::default()
    };
    db.sync().claim(REMOTE, &limits).expect("claim")
}

fn acknowledge(db: &Db, envelope: &Envelope, server_version: Value) {
    db.sync()
        .apply_push_result(
            REMOTE,
            &PushResult {
                lease_id: None,
                acknowledged: vec![Acknowledgement::of(envelope, server_version)],
                rejected: Vec::new(),
            },
        )
        .expect("acknowledge");
}

fn state(db: &Db, id: &str) -> StateKind {
    db.sync()
        .state_of("notes", &json!(id))
        .expect("state")
        .expect("tracked")
        .state
}

fn pending(db: &Db) -> u64 {
    db.sync().status(REMOTE).expect("status").pending
}

fn code(error: &EngineError) -> &'static str {
    error.code()
}

fn pull(db: &Db, expected: Value, next: Value, changes: Vec<RemoteChange>) {
    db.sync()
        .apply_remote(
            REMOTE,
            &RemotePage {
                expected_checkpoint: expected,
                next_checkpoint: next,
                changes,
            },
        )
        .expect("apply remote");
}

fn remote_upsert(id: &str, title: &str, version: &str) -> RemoteChange {
    RemoteChange {
        table: "notes".to_string(),
        key: json!(id),
        operation: Operation::Upsert,
        row: Some(json!({"id": id, "title": title})),
        server_version: json!(version),
        mutation_id: None,
    }
}

// I1: a committed write to a synchronized table leaves a durable debt.
#[test]
fn a_write_to_a_synced_table_records_its_change_in_the_same_commit() {
    let dir = TestDir::new("sync_i1");
    let db = open(&dir);

    insert(&db, "a", "first");

    assert_eq!(pending(&db), 1);
    assert_eq!(state(&db, "a"), StateKind::Pending);
    let batch = claim(&db);
    assert_eq!(batch.envelopes.len(), 1);
    let envelope = &batch.envelopes[0];
    assert_eq!(envelope.table, "notes");
    assert_eq!(envelope.key, json!("a"));
    assert_eq!(envelope.operation, Operation::Upsert);
    assert_eq!(envelope.row, Some(json!({"id": "a", "title": "first"})));
    assert_eq!(envelope.local_revision, 1);
    assert_eq!(
        envelope.base_version,
        Value::Null,
        "never seen by the server"
    );
}

#[test]
fn local_only_tables_record_nothing() {
    let dir = TestDir::new("sync_local_only");
    let db = open(&dir);

    Query::insert_into("cache", [json!({"id": 1})])
        .execute(&db)
        .expect("insert");

    assert_eq!(pending(&db), 0);
    let state = db
        .sync()
        .state_of("cache", &json!(1))
        .expect("state")
        .expect("state");
    assert_eq!(state.state, StateKind::LocalOnly);
}

// I2: a rolled back transaction leaves no publishable debt.
#[test]
fn a_rolled_back_transaction_leaves_no_change_behind() {
    let dir = TestDir::new("sync_i2");
    let db = open(&dir);

    let result: Result<(), EngineError> = db.transaction(|tx| {
        Query::insert_into("notes", [json!({"id": "a", "title": "draft"})]).execute_in(tx)?;
        Err(EngineError::InvalidRequest("undo".to_string()))
    });

    assert!(result.is_err());
    assert_eq!(pending(&db), 0);
    assert!(db
        .sync()
        .state_of("notes", &json!("a"))
        .expect("state")
        .is_none());
    assert!(claim(&db).envelopes.is_empty());
}

// TX12: a savepoint rolled back takes its debt with it.
#[test]
fn a_rolled_back_savepoint_leaves_no_change() {
    let dir = TestDir::new("sync_tx12");
    let db = open(&dir);

    db.transaction(|tx| {
        Query::insert_into("notes", [json!({"id": "kept", "title": "k"})]).execute_in(tx)?;
        let undone: Result<(), EngineError> = tx.savepoint(|inner| {
            Query::insert_into("notes", [json!({"id": "undone", "title": "u"})])
                .execute_in(inner)?;
            Err(EngineError::InvalidRequest(
                "undo the savepoint".to_string(),
            ))
        });
        assert!(undone.is_err());
        Ok(())
    })
    .expect("transaction");

    let keys: Vec<Value> = claim(&db).envelopes.into_iter().map(|e| e.key).collect();
    assert_eq!(keys, [json!("kept")]);
    assert_eq!(pending(&db), 1);
}

// TX13: if the change cannot be recorded, the row is not written either.
#[test]
fn a_change_that_cannot_be_recorded_fails_the_write() {
    let dir = TestDir::new("sync_tx13");
    let db = open(&dir);
    insert(&db, "a", "first");
    Query::delete("notes")
        .filter(col("id").eq("a"))
        .execute(&db)
        .expect("delete");

    // The deletion is not settled: recreating the row cannot be recorded.
    let error = Query::insert_into("notes", [json!({"id": "a", "title": "again"})])
        .execute(&db)
        .expect_err("recreate");

    assert_eq!(code(&error), "TombstonePending");
    assert!(note(&db, "a").is_none(), "the row was not written");
}

#[test]
fn changes_of_one_transaction_share_a_local_transaction_id() {
    let dir = TestDir::new("sync_ltx");
    let db = open(&dir);

    db.transaction(|tx| {
        Query::insert_into("notes", [json!({"id": "a", "title": "a"})]).execute_in(tx)?;
        tx.savepoint(|inner| {
            Query::insert_into("notes", [json!({"id": "b", "title": "b"})]).execute_in(inner)?;
            Ok(())
        })
    })
    .expect("transaction");
    insert(&db, "c", "c");

    let batch = claim(&db);
    let ids: Vec<&str> = batch
        .envelopes
        .iter()
        .map(|e| e.local_transaction_id.as_str())
        .collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids[0], ids[1], "same commit");
    assert_ne!(ids[0], ids[2], "another commit");
}

#[test]
fn an_update_without_a_material_change_records_nothing() {
    let dir = TestDir::new("sync_noop");
    let db = open(&dir);
    insert(&db, "a", "same");
    let first = claim(&db);
    acknowledge(&db, &first.envelopes[0], json!("v1"));

    retitle(&db, "a", "same");

    assert_eq!(pending(&db), 0);
    assert_eq!(state(&db, "a"), StateKind::Synced);
}

// I3 and TX15: the acknowledgement of revision 1 never confirms revision 2.
// I4 and I5: the envelope of revision 1 keeps its payload and identity.
#[test]
fn an_ack_of_one_revision_leaves_the_next_one_pending() {
    let dir = TestDir::new("sync_i3");
    let db = open(&dir);
    insert(&db, "a", "seven");
    let sent = claim_expired(&db).envelopes.remove(0);

    retitle(&db, "a", "eight");

    // The same mutation again, after its lease expired: same bytes.
    let resent = claim(&db).envelopes.remove(0);
    assert_eq!(resent.mutation_id, sent.mutation_id);
    assert_eq!(resent.row, Some(json!({"id": "a", "title": "seven"})));
    assert_eq!(resent.base_version, sent.base_version);
    assert_eq!(resent.attempt, 2);

    acknowledge(&db, &sent, json!("v7"));

    let entity = db
        .sync()
        .state_of("notes", &json!("a"))
        .expect("state")
        .expect("tracked");
    assert_eq!(
        entity.state,
        StateKind::Pending,
        "revision 2 is still pending"
    );
    assert_eq!(entity.acknowledged_local_revision, 1);
    assert_eq!(entity.settled_local_revision, 1);
    assert_eq!(entity.local_revision, 2);
    assert_eq!(pending(&db), 1);

    // The next envelope is prepared on the version the server returned.
    let next = claim(&db).envelopes.remove(0);
    assert_eq!(next.local_revision, 2);
    assert_eq!(next.row, Some(json!({"id": "a", "title": "eight"})));
    assert_eq!(next.base_version, json!("v7"));
    assert_eq!(next.predecessor.as_deref(), Some(sent.mutation_id.as_str()));
}

#[test]
fn only_one_delivery_per_entity_is_in_flight() {
    let dir = TestDir::new("sync_one_per_entity");
    let db = open(&dir);
    insert(&db, "a", "1");
    retitle(&db, "a", "2");
    insert(&db, "b", "1");

    let batch = claim(&db);

    let sent: Vec<(Value, u64)> = batch
        .envelopes
        .iter()
        .map(|e| (e.key.clone(), e.local_revision))
        .collect();
    assert_eq!(sent, [(json!("a"), 1), (json!("b"), 1)]);
    assert!(
        claim(&db).envelopes.is_empty(),
        "revision 2 of `a` waits for revision 1"
    );
}

// TX16: claim only sees committed changes.
#[test]
fn claim_only_sees_committed_changes() {
    let dir = TestDir::new("sync_tx16");
    let db = open(&dir);
    let writer = db.clone();
    let (written, wait) = mpsc::channel();

    let handle = thread::spawn(move || {
        let result: Result<(), EngineError> = writer.transaction(|tx| {
            Query::insert_into("notes", [json!({"id": "a", "title": "never"})]).execute_in(tx)?;
            written.send(()).expect("signal");
            thread::sleep(Duration::from_millis(100));
            Err(EngineError::InvalidRequest("roll back".to_string()))
        });
        assert!(result.is_err());
    });
    wait.recv().expect("written");

    let batch = claim(&db);

    handle.join().expect("writer");
    assert!(batch.envelopes.is_empty());
    assert_eq!(pending(&db), 0);
}

#[test]
fn a_duplicate_ack_is_idempotent_and_an_unknown_one_changes_nothing() {
    let dir = TestDir::new("sync_ack_rules");
    let db = open(&dir);
    insert(&db, "a", "1");
    insert(&db, "b", "1");
    let batch = claim(&db);

    acknowledge(&db, &batch.envelopes[0], json!("v1"));
    acknowledge(&db, &batch.envelopes[0], json!("v1"));
    assert_eq!(state(&db, "a"), StateKind::Synced);

    let mut unknown = Acknowledgement::of(&batch.envelopes[1], json!("v1"));
    unknown.mutation_id = "nobody-1".to_string();
    let error = db
        .sync()
        .apply_push_result(
            REMOTE,
            &PushResult {
                lease_id: None,
                acknowledged: vec![
                    Acknowledgement::of(&batch.envelopes[1], json!("v1")),
                    unknown,
                ],
                rejected: Vec::new(),
            },
        )
        .expect_err("unknown mutation");

    assert_eq!(code(&error), "UnknownMutation");
    assert_eq!(
        state(&db, "b"),
        StateKind::Pending,
        "the valid ack was not applied"
    );
}

#[test]
fn an_ack_of_another_revision_or_entity_is_rejected() {
    let dir = TestDir::new("sync_ack_mismatch");
    let db = open(&dir);
    insert(&db, "a", "1");
    let envelope = claim(&db).envelopes.remove(0);

    let mut wrong_revision = Acknowledgement::of(&envelope, json!("v1"));
    wrong_revision.local_revision = 2;
    let mut wrong_entity = Acknowledgement::of(&envelope, json!("v1"));
    wrong_entity.key = json!("b");

    for ack in [wrong_revision, wrong_entity] {
        let error = db
            .sync()
            .apply_push_result(
                REMOTE,
                &PushResult {
                    lease_id: None,
                    acknowledged: vec![ack],
                    rejected: Vec::new(),
                },
            )
            .expect_err("mismatch");
        assert_eq!(code(&error), "AcknowledgementMismatch");
    }
    assert_eq!(state(&db, "a"), StateKind::Pending);
}

#[test]
fn a_late_release_does_not_undo_a_newer_lease_or_an_ack() {
    let dir = TestDir::new("sync_late_release");
    let db = open(&dir);
    insert(&db, "a", "1");
    let old = claim_expired(&db);
    let new = claim(&db);
    assert_ne!(old.lease_id, new.lease_id);

    let old_lease = old.lease_id.expect("lease");
    let released = db
        .sync()
        .release(REMOTE, old_lease, "timeout")
        .expect("release");

    assert_eq!(released, 0, "the delivery moved to a newer lease");
    let listed = db.sync().pending(REMOTE, None, None).expect("pending");
    assert_eq!(listed.changes[0].state, DeliveryKind::Leased);

    // An ack after the lease expired is still the same mutation.
    acknowledge(&db, &old.envelopes[0], json!("v1"));
    let new_lease = new.lease_id.expect("lease");
    let released = db
        .sync()
        .release(REMOTE, new_lease, "late")
        .expect("release");
    assert_eq!(released, 0);
    assert_eq!(state(&db, "a"), StateKind::Synced);
}

#[test]
fn a_push_result_releases_what_it_leaves_out_and_blocks_what_cannot_be_retried() {
    let dir = TestDir::new("sync_push_result");
    let db = open(&dir);
    for id in ["a", "b", "c"] {
        insert(&db, id, "1");
    }
    let batch = claim(&db);

    let outcome = db
        .sync()
        .apply_push_result(
            REMOTE,
            &PushResult {
                lease_id: batch.lease_id,
                acknowledged: vec![Acknowledgement::of(&batch.envelopes[0], json!("v1"))],
                rejected: vec![Rejection::of(&batch.envelopes[1], "invalid title", false)],
            },
        )
        .expect("push result");

    assert_eq!(
        (outcome.acknowledged, outcome.rejected, outcome.released),
        (1, 1, 1)
    );
    assert_eq!(state(&db, "a"), StateKind::Synced);
    assert_eq!(state(&db, "b"), StateKind::Blocked);
    assert_eq!(state(&db, "c"), StateKind::Pending);
    let again: Vec<Value> = claim(&db).envelopes.into_iter().map(|e| e.key).collect();
    assert_eq!(again, [json!("c")], "a blocked change is not sent");

    let retried = db
        .sync()
        .retry(REMOTE, &[batch.envelopes[1].mutation_id.clone()])
        .expect("retry");
    assert_eq!(retried, 1);
    assert_eq!(state(&db, "b"), StateKind::Pending);
}

// I6: a deletion keeps its evidence until it is settled.
#[test]
fn a_delete_leaves_a_tombstone_until_it_is_acknowledged() {
    let dir = TestDir::new("sync_i6");
    {
        let db = open(&dir);
        insert(&db, "a", "1");
        let created = claim(&db).envelopes.remove(0);
        acknowledge(&db, &created, json!("v1"));
        Query::delete("notes")
            .filter(col("id").eq("a"))
            .execute(&db)
            .expect("delete");
        assert!(note(&db, "a").is_none(), "hidden from queries");
    }

    let db = open(&dir);
    let entity = db
        .sync()
        .state_of("notes", &json!("a"))
        .expect("state")
        .expect("tracked");
    assert_eq!(entity.state, StateKind::Pending);
    assert!(entity.deleted);
    let deletion = claim(&db).envelopes.remove(0);
    assert_eq!(deletion.operation, Operation::Delete);
    assert_eq!(deletion.row, None);
    assert_eq!(deletion.base_version, json!("v1"));
    assert_eq!(deletion.generation, 1);

    acknowledge(&db, &deletion, Value::Null);
    insert(&db, "a", "reborn");

    let reborn = claim(&db).envelopes.remove(0);
    assert_eq!(reborn.generation, 2, "a new incarnation of the key");
    assert_eq!(reborn.operation, Operation::Upsert);
}

// I7: applying remote changes does not echo them back.
#[test]
fn applying_remote_changes_writes_rows_without_an_echo() {
    let dir = TestDir::new("sync_i7");
    let db = open(&dir);

    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("r", "from server", "v3")],
    );

    assert_eq!(
        note(&db, "r"),
        Some(json!({"id": "r", "title": "from server"}))
    );
    let by_title: Vec<Value> = db
        .load(&Query::table("notes").filter(col("title").eq("from server")))
        .expect("index");
    assert_eq!(by_title.len(), 1, "indexes follow remote rows");
    assert_eq!(pending(&db), 0);
    assert!(claim(&db).envelopes.is_empty());
    let entity = db
        .sync()
        .state_of("notes", &json!("r"))
        .expect("state")
        .expect("tracked");
    assert_eq!(entity.state, StateKind::Synced);
    assert_eq!(entity.server_version, json!("v3"));
    assert_eq!(
        db.sync().status(REMOTE).expect("status").checkpoint,
        json!("cursor-1")
    );
}

// I8 and TX17: the checkpoint advances with its page, or not at all.
#[test]
fn a_page_that_fails_applies_nothing_and_keeps_the_checkpoint() {
    let dir = TestDir::new("sync_i8");
    let db = open(&dir);
    pull(&db, Value::Null, json!("cursor-1"), Vec::new());

    let stale = db.sync().apply_remote(
        REMOTE,
        &RemotePage {
            expected_checkpoint: Value::Null,
            next_checkpoint: json!("cursor-2"),
            changes: vec![remote_upsert("x", "x", "v1")],
        },
    );
    assert_eq!(code(&stale.expect_err("stale")), "StaleCheckpoint");

    let mut broken = remote_upsert("y", "y", "v2");
    broken.row = Some(json!({"id": "other", "title": "y"}));
    let halfway = db.sync().apply_remote(
        REMOTE,
        &RemotePage {
            expected_checkpoint: json!("cursor-1"),
            next_checkpoint: json!("cursor-2"),
            changes: vec![remote_upsert("x", "x", "v1"), broken],
        },
    );
    assert_eq!(code(&halfway.expect_err("broken page")), "InvalidRequest");

    assert!(note(&db, "x").is_none(), "the first change was not kept");
    assert_eq!(
        db.sync().status(REMOTE).expect("status").checkpoint,
        json!("cursor-1")
    );
}

// I9: a remote change over a pending local change is a conflict.
#[test]
fn a_pull_over_a_pending_change_keeps_the_local_row_and_records_a_conflict() {
    let dir = TestDir::new("sync_i9");
    let db = open(&dir);
    insert(&db, "a", "local");

    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("a", "remote", "v9")],
    );

    assert_eq!(note(&db, "a"), Some(json!({"id": "a", "title": "local"})));
    assert_eq!(state(&db, "a"), StateKind::Conflict);
    assert!(
        claim(&db).envelopes.is_empty(),
        "a conflict holds its changes"
    );
    let conflicts = db.sync().conflicts(REMOTE).expect("conflicts");
    assert_eq!(conflicts.len(), 1);
    let conflict = &conflicts[0];
    assert_eq!(conflict.key, json!("a"));
    assert_eq!(conflict.remote_version, json!("v9"));
    assert_eq!(
        conflict.remote_row,
        Some(json!({"id": "a", "title": "remote"}))
    );
    assert_eq!(conflict.outstanding.len(), 1);
    assert_eq!(db.sync().status(REMOTE).expect("status").conflicts, 1);
}

#[test]
fn accepting_the_remote_variant_settles_without_a_fake_ack() {
    let dir = TestDir::new("sync_accept_remote");
    let db = open(&dir);
    insert(&db, "a", "local");
    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("a", "remote", "v9")],
    );
    let conflict = db.sync().conflicts(REMOTE).expect("conflicts").remove(0);

    db.sync()
        .resolve_conflict(
            &conflict.id,
            conflict.local_row_version,
            &Resolution::AcceptRemote,
        )
        .expect("resolve");

    assert_eq!(note(&db, "a"), Some(json!({"id": "a", "title": "remote"})));
    let entity = db
        .sync()
        .state_of("notes", &json!("a"))
        .expect("state")
        .expect("tracked");
    assert_eq!(entity.state, StateKind::Synced);
    assert_eq!(
        entity.acknowledged_local_revision, 0,
        "nothing was acknowledged"
    );
    assert_eq!(entity.settled_local_revision, entity.local_revision);
    assert_eq!(entity.server_version, json!("v9"));
    assert_eq!(pending(&db), 0);
    assert!(db.sync().conflicts(REMOTE).expect("conflicts").is_empty());
}

#[test]
fn a_merge_sends_a_new_change_on_the_remote_base() {
    let dir = TestDir::new("sync_merge");
    let db = open(&dir);
    insert(&db, "a", "local");
    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("a", "remote", "v9")],
    );
    let conflict = db.sync().conflicts(REMOTE).expect("conflicts").remove(0);

    db.sync()
        .resolve_conflict(
            &conflict.id,
            conflict.local_row_version,
            &Resolution::Merged {
                row: json!({"id": "a", "title": "local + remote"}),
            },
        )
        .expect("resolve");

    assert_eq!(state(&db, "a"), StateKind::Pending);
    let merged = claim(&db).envelopes.remove(0);
    assert_eq!(
        merged.row,
        Some(json!({"id": "a", "title": "local + remote"}))
    );
    assert_eq!(merged.base_version, json!("v9"));
    assert_eq!(note(&db, "a"), merged.row);
}

#[test]
fn a_resolution_on_a_stale_row_version_changes_nothing() {
    let dir = TestDir::new("sync_stale_resolution");
    let db = open(&dir);
    insert(&db, "a", "local");
    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("a", "remote", "v9")],
    );
    let conflict = db.sync().conflicts(REMOTE).expect("conflicts").remove(0);
    retitle(&db, "a", "edited after the conflict");

    let error = db
        .sync()
        .resolve_conflict(
            &conflict.id,
            conflict.local_row_version,
            &Resolution::AcceptRemote,
        )
        .expect_err("stale");

    assert_eq!(code(&error), "RowVersionMismatch");
    assert_eq!(
        note(&db, "a"),
        Some(json!({"id": "a", "title": "edited after the conflict"}))
    );
    assert_eq!(state(&db, "a"), StateKind::Conflict);
}

#[test]
fn a_resolution_while_a_change_is_being_sent_is_refused() {
    let dir = TestDir::new("sync_in_flight");
    let db = open(&dir);
    insert(&db, "a", "local");
    let sent = claim(&db);
    pull(
        &db,
        Value::Null,
        json!("cursor-1"),
        vec![remote_upsert("a", "remote", "v9")],
    );
    let conflict = db.sync().conflicts(REMOTE).expect("conflicts").remove(0);

    let error = db
        .sync()
        .resolve_conflict(
            &conflict.id,
            conflict.local_row_version,
            &Resolution::AcceptRemote,
        )
        .expect_err("in flight");

    assert_eq!(code(&error), "MutationInFlight");
    let released = db
        .sync()
        .release(REMOTE, sent.lease_id.expect("lease"), "network")
        .expect("release");
    assert_eq!(released, 1);
    db.sync()
        .resolve_conflict(
            &conflict.id,
            conflict.local_row_version,
            &Resolution::AcceptRemote,
        )
        .expect("resolve after release");
}

#[test]
fn an_echo_of_a_local_change_acknowledges_it() {
    let dir = TestDir::new("sync_echo");
    let db = open(&dir);
    insert(&db, "a", "mine");
    let sent = claim(&db).envelopes.remove(0);

    let mut echo = remote_upsert("a", "mine", "v4");
    echo.mutation_id = Some(sent.mutation_id.clone());
    pull(&db, Value::Null, json!("cursor-1"), vec![echo]);

    assert_eq!(state(&db, "a"), StateKind::Synced);
    assert!(db.sync().conflicts(REMOTE).expect("conflicts").is_empty());
}

// I12: rows written before the table was synchronized are unknown.
#[test]
fn rows_from_before_sync_are_unknown_and_sync_cannot_be_turned_off() {
    let dir = TestDir::new("sync_i12");
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(TableDef::new("notes", "id"))
        .expect("define");
    Query::insert_into("notes", [json!({"id": "old", "title": "before"})])
        .execute(&db)
        .expect("insert");

    db.define_table(TableDef::new("notes", "id").sync_with(REMOTE))
        .expect("enable sync");

    assert_eq!(state(&db, "old"), StateKind::Unknown);
    let error = db
        .define_table(TableDef::new("notes", "id"))
        .expect_err("turn off");
    assert_eq!(code(&error), "SchemaMismatch");
}

// §19.4 "two workers": concurrent claims never lease the same change twice.
#[test]
fn two_workers_claiming_at_once_get_disjoint_batches() {
    let dir = TestDir::new("sync_two_workers");
    let db = open(&dir);
    for i in 0..40 {
        insert(&db, &format!("n{i:02}"), "x");
    }
    let limits = ClaimLimits {
        max_changes: 25,
        ..ClaimLimits::default()
    };

    let workers: Vec<_> = (0..2)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || db.sync().claim(REMOTE, &limits).expect("claim"))
        })
        .collect();
    let batches: Vec<ClaimedBatch> = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect();

    let mut keys: Vec<Value> = batches
        .iter()
        .flat_map(|batch| batch.envelopes.iter().map(|e| e.key.clone()))
        .collect();
    assert_eq!(keys.len(), 40, "25 + 15: every change once");
    keys.sort_by_key(Value::to_string);
    keys.dedup();
    assert_eq!(keys.len(), 40, "no change leased twice");
    assert_ne!(batches[0].lease_id, batches[1].lease_id);
}

// §19.4 "new relations": a row and its link are sent in commit order, and
// carry the same local transaction.
#[test]
fn a_row_and_its_link_keep_their_order_and_their_transaction() {
    let dir = TestDir::new("sync_relations");
    let db = open(&dir);
    db.define_table(
        TableDef::new("note_tags", "id")
            .index("by_note", &["note"])
            .sync_with(REMOTE),
    )
    .expect("define links");

    db.transaction(|tx| {
        Query::insert_into("notes", [json!({"id": "a", "title": "t"})]).execute_in(tx)?;
        Query::insert_into(
            "note_tags",
            [json!({"id": "a/rust", "note": "a", "tag": "rust"})],
        )
        .execute_in(tx)?;
        Ok(())
    })
    .expect("transaction");

    let batch = claim(&db);
    let tables: Vec<&str> = batch.envelopes.iter().map(|e| e.table.as_str()).collect();
    assert_eq!(tables, ["notes", "note_tags"], "the row before its link");
    assert_eq!(
        batch.envelopes[0].local_transaction_id,
        batch.envelopes[1].local_transaction_id
    );
}

// §19.4 "migration with a pending outbox": changing the schema keeps the
// recorded payloads as they were written.
#[test]
fn a_schema_change_keeps_the_pending_payloads() {
    let dir = TestDir::new("sync_schema_change");
    let db = open(&dir);
    insert(&db, "a", "before");

    db.define_table(
        TableDef::new("notes", "id")
            .index("by_title", &["title"])
            .index("by_title_and_id", &["title", "id"])
            .sync_with(REMOTE),
    )
    .expect("add an index");

    assert_eq!(state(&db, "a"), StateKind::Pending);
    let envelope = claim(&db).envelopes.remove(0);
    assert_eq!(envelope.row, Some(json!({"id": "a", "title": "before"})));
}
