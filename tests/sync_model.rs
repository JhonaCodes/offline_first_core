//! A small reference model of the offline-first sync (RFC-001 §19.4),
//! compared with the engine over random sequences of local writes and
//! deletes, claims, partial and lost responses, duplicate acknowledgements,
//! remote writes of another device, pulls, conflict resolutions and
//! restarts. After every step the engine must agree with the model, row by
//! row; at the end, once the network settles, every row must equal the
//! server's, and no acknowledged write may be missing from the server.
//!
//! The simulated server applies a change only on the version it was based
//! on, deduplicates by mutation id, and keeps a feed of every change for
//! pulls.

mod common;

use std::collections::{HashMap, HashSet};

use common::TestDir;
use offline_first_core::engine::sync::{
    Acknowledgement, ClaimLimits, Envelope, Operation, PushResult, Rejection, RemoteChange,
    RemotePage, Resolution, StateKind,
};
use offline_first_core::engine::{col, Db, Durability, OpenOptions, Query, TableDef};
use serde_json::{json, Value};

/// Sequences run, and steps in each: the N of task 06/05.
const SEQUENCES: u64 = 150;
const STEPS: usize = 80;
const KEYS: u64 = 4;
const REMOTE: &str = "primary";
const LONG_LEASE: u64 = 60_000;

/// xorshift64*: deterministic, so a failing seed can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn version(n: u64) -> Value {
    json!(format!("s{n}"))
}

struct FeedEntry {
    key: String,
    row: Option<Value>,
    version: u64,
    mutation_id: Option<String>,
}

enum Answer {
    Ack(Value),
    Conflict,
}

/// The server: conditional writes, deduplicated by mutation id.
#[derive(Default)]
struct Server {
    rows: HashMap<String, (Option<Value>, u64)>,
    applied: HashMap<String, u64>,
    feed: Vec<FeedEntry>,
    next_version: u64,
}

impl Server {
    fn current(&self, key: &str) -> Value {
        self.rows.get(key).map_or(Value::Null, |(_, v)| version(*v))
    }

    fn write(&mut self, key: &str, row: Option<Value>, mutation_id: Option<String>) -> u64 {
        self.next_version += 1;
        let v = self.next_version;
        self.rows.insert(key.to_string(), (row.clone(), v));
        self.feed.push(FeedEntry {
            key: key.to_string(),
            row,
            version: v,
            mutation_id,
        });
        v
    }

    fn push(&mut self, envelope: &Envelope) -> Answer {
        if let Some(v) = self.applied.get(&envelope.mutation_id) {
            return Answer::Ack(version(*v));
        }
        let key = envelope.key.as_str().expect("string keys").to_string();
        if envelope.base_version != self.current(&key) {
            return Answer::Conflict;
        }
        let v = self.write(
            &key,
            envelope.row.clone(),
            Some(envelope.mutation_id.clone()),
        );
        self.applied.insert(envelope.mutation_id.clone(), v);
        Answer::Ack(version(v))
    }
}

/// One open local change, as the model sees it.
#[derive(Clone)]
struct Rev {
    revision: u64,
    sequence: u64,
    row: Option<Value>,
    mutation_id: Option<String>,
    base: Option<Value>,
    lease: Option<u64>,
    alive: bool,
}

#[derive(Default)]
struct Entity {
    row: Option<Value>,
    revision: u64,
    open: Vec<Rev>,
    server_version: Value,
    initialized: bool,
    conflict: Option<(Value, Option<Value>)>,
    touched: bool,
}

#[derive(Default)]
struct Model {
    entities: HashMap<String, Entity>,
    sequence: u64,
    settled: HashSet<String>,
    acknowledged: HashSet<String>,
    checkpoint: usize,
}

impl Model {
    fn entity(&mut self, key: &str) -> &mut Entity {
        self.entities.entry(key.to_string()).or_default()
    }

    fn record(&mut self, key: &str, row: Option<Value>) {
        self.sequence += 1;
        let sequence = self.sequence;
        let entity = self.entity(key);
        entity.revision += 1;
        entity.touched = true;
        let revision = entity.revision;
        entity.open.push(Rev {
            revision,
            sequence,
            row,
            mutation_id: None,
            base: None,
            lease: None,
            alive: false,
        });
    }

    fn settle_first(&mut self, key: &str, server_version: Value) {
        let entity = self.entity(key);
        let rev = entity.open.remove(0);
        entity.server_version = server_version;
        entity.initialized = true;
        let id = rev.mutation_id.expect("an acknowledged change was sent");
        self.settled.insert(id.clone());
        self.acknowledged.insert(id);
    }

    fn release(&mut self, lease: u64) {
        for entity in self.entities.values_mut() {
            for rev in &mut entity.open {
                if rev.lease == Some(lease) {
                    rev.lease = None;
                    rev.alive = false;
                }
            }
        }
    }

    fn kind(entity: &Entity) -> StateKind {
        if entity.conflict.is_some() {
            StateKind::Conflict
        } else if !entity.open.is_empty() {
            StateKind::Pending
        } else if entity.initialized {
            StateKind::Synced
        } else {
            StateKind::Unknown
        }
    }
}

/// How often each path ran, over all sequences: a model test that stops
/// reaching a path must fail, not pass quietly.
#[derive(Debug, Default)]
struct Coverage {
    tombstones: u64,
    duplicates: u64,
    echoes: u64,
    conflicts: u64,
    resolutions: u64,
    in_flight: u64,
    lost_answers: u64,
}

struct Run {
    dir: TestDir,
    db: Option<Db>,
    model: Model,
    server: Server,
    rng: Rng,
    counter: u64,
    seed: u64,
    coverage: Coverage,
}

fn open(dir: &TestDir) -> Db {
    let options = OpenOptions {
        durability: Durability::NoSync,
        ..OpenOptions::default()
    };
    let db = Db::open_with(dir.db_dir("db"), options).expect("open");
    db.define_table(TableDef::new("notes", "id").sync_with(REMOTE))
        .expect("define");
    db
}

impl Run {
    fn new(seed: u64) -> Self {
        let dir = TestDir::new(&format!("sync_model_{seed}"));
        let db = open(&dir);
        Self {
            dir,
            db: Some(db),
            model: Model::default(),
            server: Server::default(),
            rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
            counter: 0,
            seed,
            coverage: Coverage::default(),
        }
    }

    fn db(&self) -> &Db {
        self.db.as_ref().expect("open database")
    }

    fn key(&mut self) -> String {
        format!("k{}", self.rng.below(KEYS))
    }

    fn title(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}-{}", self.counter)
    }

    fn local_write(&mut self) {
        let key = self.key();
        let current = self.model.entity(&key).row.clone();
        let tombstone = current.is_none() && !self.model.entity(&key).open.is_empty();
        if current.is_some() && self.rng.chance(25) {
            Query::delete("notes")
                .filter(col("id").eq(key.as_str()))
                .execute(self.db())
                .expect("delete");
            self.model.entity(&key).row = None;
            self.model.record(&key, None);
            return;
        }
        let title = self.title("local");
        let row = json!({"id": key, "title": title});
        if current.is_some() {
            Query::update("notes")
                .filter(col("id").eq(key.as_str()))
                .set("title", title)
                .execute(self.db())
                .expect("update");
        } else {
            let inserted = Query::insert_into("notes", [row.clone()]).execute(self.db());
            if tombstone {
                let error = inserted.expect_err("a pending deletion blocks the key");
                assert_eq!(error.code(), "TombstonePending", "seed {}", self.seed);
                self.coverage.tombstones += 1;
                return;
            }
            inserted.expect("insert");
        }
        self.model.entity(&key).row = Some(row.clone());
        self.model.record(&key, Some(row));
    }

    /// The envelopes the model expects a claim to return, in commit order.
    fn eligible(&self) -> Vec<(String, u64)> {
        let mut eligible: Vec<(u64, String, u64)> = self
            .model
            .entities
            .iter()
            .filter(|(_, entity)| entity.conflict.is_none())
            .filter_map(|(key, entity)| {
                entity
                    .open
                    .first()
                    .filter(|rev| !rev.alive)
                    .map(|rev| (rev.sequence, key.clone(), rev.revision))
            })
            .collect();
        eligible.sort();
        eligible
            .into_iter()
            .map(|(_, key, revision)| (key, revision))
            .collect()
    }

    /// Marks the claimed changes in the model, checking that their bytes,
    /// identity and base never move (I4, I5).
    fn note_claimed(&mut self, envelopes: &[Envelope], lease: u64, alive: bool) {
        for envelope in envelopes {
            let key = envelope.key.as_str().expect("key").to_string();
            let entity = self.model.entity(&key);
            let server_version = entity.server_version.clone();
            let rev = &mut entity.open[0];
            assert_eq!(envelope.row, rev.row, "seed {}: payload", self.seed);
            let id = rev
                .mutation_id
                .get_or_insert_with(|| envelope.mutation_id.clone());
            assert_eq!(*id, envelope.mutation_id, "seed {}: identity", self.seed);
            let base = rev.base.get_or_insert(server_version);
            assert_eq!(*base, envelope.base_version, "seed {}: base", self.seed);
            rev.lease = Some(lease);
            rev.alive = alive;
        }
    }

    fn push(&mut self) {
        let long = self.rng.chance(70);
        let limits = ClaimLimits {
            lease_ms: if long { LONG_LEASE } else { 0 },
            ..ClaimLimits::default()
        };
        let expected = self.eligible();
        let batch = self.db().sync().claim(REMOTE, &limits).expect("claim");
        let claimed: Vec<(String, u64)> = batch
            .envelopes
            .iter()
            .map(|e| (e.key.as_str().expect("key").to_string(), e.local_revision))
            .collect();
        assert_eq!(claimed, expected, "seed {}: claim", self.seed);
        let Some(lease) = batch.lease_id else {
            return;
        };
        self.note_claimed(&batch.envelopes, lease, long);

        let mut result = PushResult {
            lease_id: Some(lease),
            ..PushResult::default()
        };
        let mut delivered_any = false;
        for envelope in &batch.envelopes {
            let answer = if self.rng.chance(10) {
                None // never reached the server
            } else {
                Some(self.server.push(envelope))
            };
            if !self.rng.chance(75) {
                continue; // the answer is lost
            }
            delivered_any = true;
            match answer {
                Some(Answer::Ack(server_version)) => {
                    result
                        .acknowledged
                        .push(Acknowledgement::of(envelope, server_version));
                }
                Some(Answer::Conflict) | None => {
                    result
                        .rejected
                        .push(Rejection::of(envelope, "retry later", true));
                }
            }
        }

        if !delivered_any {
            self.coverage.lost_answers += 1;
            if self.rng.chance(50) {
                return; // still waiting: a long lease stays in flight
            }
            let released = self
                .db()
                .sync()
                .release(REMOTE, lease, "timeout")
                .expect("release");
            assert_eq!(
                released,
                batch.envelopes.len() as u64,
                "seed {}: release",
                self.seed
            );
            self.model.release(lease);
            return;
        }
        let outcome = self
            .db()
            .sync()
            .apply_push_result(REMOTE, &result)
            .expect("push result");
        assert_eq!(
            outcome.acknowledged,
            result.acknowledged.len() as u64,
            "seed {}: acknowledged",
            self.seed
        );
        for ack in &result.acknowledged {
            let key = ack.key.as_str().expect("key").to_string();
            self.model.settle_first(&key, ack.server_version.clone());
        }
        self.model.release(lease);

        // A duplicate acknowledgement changes nothing.
        if let Some(ack) = result.acknowledged.first() {
            if self.rng.chance(30) {
                let again = self
                    .db()
                    .sync()
                    .apply_push_result(
                        REMOTE,
                        &PushResult {
                            lease_id: None,
                            acknowledged: vec![ack.clone()],
                            rejected: Vec::new(),
                        },
                    )
                    .expect("duplicate ack");
                assert_eq!(again.acknowledged, 0, "seed {}: duplicate", self.seed);
                self.coverage.duplicates += 1;
            }
        }
    }

    fn remote_write(&mut self) {
        let key = self.key();
        let exists = self
            .server
            .rows
            .get(&key)
            .is_some_and(|(row, _)| row.is_some());
        let row = if exists && self.rng.chance(30) {
            None
        } else {
            let title = self.title("remote");
            Some(json!({"id": key, "title": title}))
        };
        self.server.write(&key, row, None);
    }

    fn checkpoint(index: usize) -> Value {
        match index {
            0 => Value::Null,
            n => json!(n),
        }
    }

    fn pull(&mut self, size: usize) {
        let start = self.model.checkpoint;
        let end = start.saturating_add(size).min(self.server.feed.len());
        if start == end {
            return;
        }
        let changes: Vec<RemoteChange> = self.server.feed[start..end]
            .iter()
            .map(|entry| RemoteChange {
                table: "notes".to_string(),
                key: json!(entry.key),
                operation: match entry.row {
                    Some(_) => Operation::Upsert,
                    None => Operation::Delete,
                },
                row: entry.row.clone(),
                server_version: version(entry.version),
                mutation_id: entry.mutation_id.clone(),
            })
            .collect();
        self.db()
            .sync()
            .apply_remote(
                REMOTE,
                &RemotePage {
                    expected_checkpoint: Self::checkpoint(start),
                    next_checkpoint: Self::checkpoint(end),
                    changes: changes.clone(),
                },
            )
            .expect("apply remote");
        self.model.checkpoint = end;
        for change in changes {
            self.model_apply(change);
        }
    }

    /// The engine's rules for one remote change, as the model states them.
    fn model_apply(&mut self, change: RemoteChange) {
        let key = change.key.as_str().expect("key").to_string();
        if let Some(id) = &change.mutation_id {
            let entity = self.model.entity(&key);
            let echo = entity
                .open
                .first()
                .is_some_and(|rev| rev.mutation_id.as_ref() == Some(id));
            if echo {
                self.model.settle_first(&key, change.server_version);
                self.coverage.echoes += 1;
                return;
            }
            if self.model.settled.contains(id) {
                return;
            }
        }
        let entity = self.model.entity(&key);
        let quiet = entity.open.is_empty() && entity.conflict.is_none();
        if quiet && entity.initialized && entity.server_version == change.server_version {
            return;
        }
        if !quiet {
            entity.conflict = Some((change.server_version, change.row));
            self.coverage.conflicts += 1;
            return;
        }
        entity.touched = true;
        entity.row = change.row;
        entity.server_version = change.server_version;
        entity.initialized = true;
    }

    fn resolve(&mut self, accept_remote_only: bool) {
        let conflicts = self.db().sync().conflicts(REMOTE).expect("conflicts");
        for conflict in conflicts {
            let key = conflict.key.as_str().expect("key").to_string();
            let entity = self.model.entity(&key);
            let (remote_version, remote_row) =
                entity.conflict.clone().expect("the model has the conflict");
            assert_eq!(
                conflict.remote_version, remote_version,
                "seed {}",
                self.seed
            );
            let in_flight = entity.open.iter().any(|rev| rev.alive);
            let choice = if accept_remote_only {
                0
            } else {
                self.rng.below(3)
            };
            let merged = json!({"id": key, "title": format!("merged-{}", self.counter + 1)});
            let resolution = match choice {
                0 => Resolution::AcceptRemote,
                1 => Resolution::KeepLocal,
                _ => Resolution::Merged {
                    row: merged.clone(),
                },
            };
            let result = self.db().sync().resolve_conflict(
                &conflict.id,
                conflict.local_row_version,
                &resolution,
            );
            if in_flight {
                assert_eq!(
                    result.expect_err("in flight").code(),
                    "MutationInFlight",
                    "seed {}",
                    self.seed
                );
                self.coverage.in_flight += 1;
                continue;
            }
            result.expect("resolve");
            self.coverage.resolutions += 1;
            self.counter += 1;
            let entity = self.model.entity(&key);
            let resolved: Vec<String> = entity
                .open
                .drain(..)
                .filter_map(|rev| rev.mutation_id)
                .collect();
            self.model.settled.extend(resolved);
            let entity = self.model.entity(&key);
            entity.conflict = None;
            entity.server_version = remote_version;
            entity.initialized = true;
            let resend = match resolution {
                Resolution::AcceptRemote => {
                    entity.row = remote_row;
                    None
                }
                Resolution::KeepLocal => match (&entity.row, &remote_row) {
                    (None, None) => None,
                    (Some(row), _) => Some(Some(row.clone())),
                    (None, Some(_)) => Some(None),
                },
                Resolution::Merged { .. } => {
                    entity.row = Some(merged.clone());
                    Some(Some(merged))
                }
            };
            if let Some(row) = resend {
                self.model.record(&key, row);
            }
        }
    }

    fn restart(&mut self) {
        self.db = None;
        self.db = Some(open(&self.dir));
    }

    /// The engine agrees with the model, row by row.
    fn check(&self, step: &str) {
        let mut pending = 0;
        for (key, entity) in &self.model.entities {
            let row: Option<Value> = self
                .db()
                .first(&Query::table("notes").filter(col("id").eq(key.as_str())))
                .expect("row");
            assert_eq!(
                row, entity.row,
                "seed {} after {step}: row {key}",
                self.seed
            );
            let state = self
                .db()
                .sync()
                .state_of("notes", &json!(key))
                .expect("state");
            let Some(state) = state else {
                assert!(
                    !entity.touched || (entity.row.is_none() && !entity.initialized),
                    "seed {} after {step}: {key} has no records",
                    self.seed
                );
                continue;
            };
            pending += entity.open.len() as u64;
            assert_eq!(
                (state.state, state.pending, state.local_revision),
                (
                    Model::kind(entity),
                    entity.open.len() as u64,
                    entity.revision
                ),
                "seed {} after {step}: state of {key}",
                self.seed
            );
            assert_eq!(
                state.server_version, entity.server_version,
                "seed {} after {step}: server version of {key}",
                self.seed
            );
        }
        let status = self.db().sync().status(REMOTE).expect("status");
        assert_eq!(status.pending, pending, "seed {} after {step}", self.seed);
    }

    fn step(&mut self) -> &'static str {
        match self.rng.below(12) {
            0..=3 => {
                self.local_write();
                "write"
            }
            4..=6 => {
                self.push();
                "push"
            }
            7 | 8 => {
                let size = 1 + self.rng.below(3) as usize;
                self.pull(size);
                "pull"
            }
            9 => {
                self.remote_write();
                "remote write"
            }
            10 => {
                self.resolve(false);
                "resolve"
            }
            _ => {
                self.restart();
                "restart"
            }
        }
    }

    /// The network comes back and stays: everything settles on the server.
    fn settle(&mut self) {
        for _ in 0..100 {
            let status = self.db().sync().status(REMOTE).expect("status");
            let behind = self.model.checkpoint < self.server.feed.len();
            if status.pending == 0 && status.conflicts == 0 && !behind {
                return;
            }
            self.pull(usize::MAX);
            self.resolve(true);
            let batch = self
                .db()
                .sync()
                .claim(REMOTE, &ClaimLimits::default())
                .expect("claim");
            if let Some(lease) = batch.lease_id {
                self.note_claimed(&batch.envelopes, lease, false);
            }
            let mut result = PushResult {
                lease_id: batch.lease_id,
                ..PushResult::default()
            };
            for envelope in &batch.envelopes {
                match self.server.push(envelope) {
                    Answer::Ack(v) => result.acknowledged.push(Acknowledgement::of(envelope, v)),
                    Answer::Conflict => {
                        result.rejected.push(Rejection::of(envelope, "stale", true));
                    }
                }
            }
            if batch.lease_id.is_some() {
                self.db()
                    .sync()
                    .apply_push_result(REMOTE, &result)
                    .expect("push result");
                for ack in &result.acknowledged {
                    let key = ack.key.as_str().expect("key").to_string();
                    self.model.settle_first(&key, ack.server_version.clone());
                }
                self.model.release(batch.lease_id.unwrap_or_default());
            }
        }
        panic!("seed {}: the sync did not settle", self.seed);
    }
}

#[test]
fn random_sequences_agree_with_the_reference_model_and_converge() {
    let mut total = Coverage::default();
    for seed in 1..=SEQUENCES {
        let mut run = Run::new(seed);
        for _ in 0..STEPS {
            let step = run.step();
            run.check(step);
        }
        // Leases held by an answer that never comes expire.
        for entity in run.model.entities.values_mut() {
            for rev in &mut entity.open {
                rev.alive = false;
            }
        }
        let leases: HashSet<u64> = run
            .model
            .entities
            .values()
            .flat_map(|e| e.open.iter().filter_map(|rev| rev.lease))
            .collect();
        for lease in leases {
            run.db()
                .sync()
                .release(REMOTE, lease, "expired")
                .expect("release");
        }
        run.settle();
        run.check("settle");

        for key in (0..KEYS).map(|k| format!("k{k}")) {
            let local: Option<Value> = run
                .db()
                .first(&Query::table("notes").filter(col("id").eq(key.as_str())))
                .expect("row");
            let server = run.server.rows.get(&key).and_then(|(row, _)| row.clone());
            assert_eq!(local, server, "seed {seed}: {key} converged");
        }
        for id in &run.model.acknowledged {
            assert!(
                run.server.applied.contains_key(id),
                "seed {seed}: acknowledged {id} is on the server"
            );
        }
        let c = &run.coverage;
        total.tombstones += c.tombstones;
        total.duplicates += c.duplicates;
        total.echoes += c.echoes;
        total.conflicts += c.conflicts;
        total.resolutions += c.resolutions;
        total.in_flight += c.in_flight;
        total.lost_answers += c.lost_answers;
    }
    println!("{SEQUENCES} sequences of {STEPS} steps: {total:?}");
    let reached = [
        total.tombstones,
        total.duplicates,
        total.echoes,
        total.conflicts,
        total.resolutions,
        total.in_flight,
        total.lost_answers,
    ];
    assert!(
        reached.iter().all(|&n| n > 0),
        "a path was never reached: {total:?}"
    );
}
