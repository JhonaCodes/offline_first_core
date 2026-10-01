//! Offline-first sync inside the transaction (RFC-001 §13).
//!
//! A table declared with [`TableDef::sync_with`](super::TableDef::sync_with)
//! records every effective write as an immutable *change* in the same LMDB
//! transaction as the row (an outbox), so a crash can never keep the row and
//! lose its debt, or the other way around. A rolled back savepoint takes its
//! changes with it, because they live in the same nested transaction.
//!
//! The engine knows what is pending, what is being sent (leased) and what
//! the server confirmed, per mutation and revision; the network, the login
//! and the retries belong to the application:
//!
//! 1. [`SyncApi::claim`] leases a batch of envelopes in a short transaction
//!    and returns them; no transaction stays open while they are sent.
//! 2. [`SyncApi::apply_push_result`] records what the server answered: an
//!    acknowledgement settles exactly one mutation, never the next revision.
//! 3. [`SyncApi::apply_remote`] applies a page of server changes and its
//!    checkpoint in one transaction, without echoing them back. A server
//!    change over a pending local change becomes a conflict, which
//!    [`SyncApi::resolve_conflict`] closes with a row version precondition.
//!
//! Rules that make it correct (RFC §13.19, invariants I1–I12):
//!
//! - One delivery in flight per entity, in revision order: revision 8 is
//!   claimed only after revision 7 was settled.
//! - An envelope fixes its `base_version` (the server version it builds on)
//!   when it is first claimed, and keeps its bytes on every retry.
//! - A late release of an old lease changes nothing: transitions check the
//!   lease id.
//! - A deletion keeps its evidence (a tombstone in the sync records) until
//!   the server acknowledged it; the key cannot be reused meanwhile.
//!
//! Storage: one internal LMDB database, `__sync`, created with the first
//! synchronized table, holding per-entity metadata, the open changes, an
//! index of open changes per remote, receipts of settled mutations, the
//! conflicts, the remote checkpoints and counters.

use std::collections::hash_map::RandomState;
use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use natdb::{Cursor, Database, RwTransaction, Transaction, WriteFlags};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::db::Db;
use super::error::{EngineError, EngineResult, SyncError};
use super::exec;
use super::keys::encode_key;
use super::store::{Store, Table};
use super::tx::WriterGuard;
use super::value::field;

/// Name of the internal database holding the sync records.
pub(crate) const SYNC_DB_NAME: &str = "__sync";

const META: u8 = b'm';
const CHANGE: u8 = b'c';
const OPEN: u8 = b'o';
const MUTATION: u8 = b'x';
const REMOTE: u8 = b'r';
const CONFLICT: u8 = b'k';
const COUNTER: u8 = b'n';

/// What a change does to its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// The row exists with the payload.
    Upsert,
    /// The row was deleted.
    Delete,
}

/// The delivery state of an open change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// Waiting to be claimed.
    Pending,
    /// Claimed under a lease that has not expired.
    Leased,
    /// Rejected by the server as not retryable; [`SyncApi::retry`] sends it
    /// again.
    Blocked,
}

/// The sync state of an entity (RFC §13.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    /// The table is not synchronized.
    LocalOnly,
    /// The row exists, but nothing says what the server holds (it was
    /// written before the table was synchronized).
    Unknown,
    /// Local changes are not settled yet.
    Pending,
    /// No pending change and no conflict, as far as the last server state
    /// observed.
    Synced,
    /// A server change met a pending local change; it needs a resolution.
    Conflict,
    /// The next change was rejected and will not be sent until retried.
    Blocked,
}

/// Limits of a [`SyncApi::claim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaimLimits {
    /// Most envelopes in the batch.
    pub max_changes: u64,
    /// Most bytes of envelopes (their JSON) in the batch; an envelope larger
    /// than this alone is still sent, alone, so it is never stuck.
    pub max_bytes: Option<u64>,
    /// How long the lease lasts, in milliseconds. After it expires, the
    /// envelopes can be claimed again (the same bytes).
    pub lease_ms: u64,
}

impl Default for ClaimLimits {
    fn default() -> Self {
        Self {
            max_changes: 100,
            max_bytes: None,
            lease_ms: 30_000,
        }
    }
}

/// One change to send, with everything the server needs to apply it once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Stable identity of the mutation: the same on every retry. Servers
    /// deduplicate by it.
    pub mutation_id: String,
    /// The table.
    pub table: String,
    /// The primary key of the row.
    pub key: Value,
    /// Incarnation of the key: grows when a deleted key is created again.
    pub generation: u64,
    /// Local revision of the entity this change produced.
    pub local_revision: u64,
    /// Changes committed together share it (no remote atomicity implied).
    pub local_transaction_id: String,
    /// Upsert or delete.
    pub operation: Operation,
    /// The row as written (immutable), or `None` for a delete.
    pub row: Option<Value>,
    /// The server version this change builds on (`null` if the server never
    /// confirmed the entity), fixed at the first claim.
    pub base_version: Value,
    /// The previous mutation of the entity, if any.
    pub predecessor: Option<String>,
    /// How many times the change was claimed, this one included.
    pub attempt: u64,
}

/// A leased batch of envelopes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimedBatch {
    /// The lease, or `None` when nothing was eligible.
    pub lease_id: Option<u64>,
    /// The envelopes, in commit order.
    pub envelopes: Vec<Envelope>,
}

/// The server confirmed one mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Acknowledgement {
    /// The mutation.
    pub mutation_id: String,
    /// Its table, checked against the change.
    pub table: String,
    /// Its key, checked against the change.
    pub key: Value,
    /// Its local revision, checked against the change.
    pub local_revision: u64,
    /// The version the server stored (opaque).
    pub server_version: Value,
}

impl Acknowledgement {
    /// The acknowledgement of `envelope`, which the server stored as
    /// `server_version`.
    pub fn of(envelope: &Envelope, server_version: Value) -> Self {
        Self {
            mutation_id: envelope.mutation_id.clone(),
            table: envelope.table.clone(),
            key: envelope.key.clone(),
            local_revision: envelope.local_revision,
            server_version,
        }
    }
}

/// The server refused one mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    /// The mutation.
    pub mutation_id: String,
    /// Why, for diagnostics.
    pub reason: String,
    /// `true`: send it again later; `false`: block it until retried.
    pub retryable: bool,
}

impl Rejection {
    /// The rejection of `envelope`.
    pub fn of(envelope: &Envelope, reason: impl Into<String>, retryable: bool) -> Self {
        Self {
            mutation_id: envelope.mutation_id.clone(),
            reason: reason.into(),
            retryable,
        }
    }
}

/// What the server answered to a push.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PushResult {
    /// The lease of the batch. With it, the envelopes of the lease that the
    /// result does not mention are released (pending again); rejections
    /// apply only to deliveries still held by it.
    pub lease_id: Option<u64>,
    /// Confirmed mutations; valid even after their lease expired.
    pub acknowledged: Vec<Acknowledgement>,
    /// Refused mutations.
    pub rejected: Vec<Rejection>,
}

/// What [`SyncApi::apply_push_result`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PushOutcome {
    /// Mutations settled by an acknowledgement (duplicates not counted).
    pub acknowledged: u64,
    /// Rejections applied.
    pub rejected: u64,
    /// Deliveries of the lease released because the result left them out.
    pub released: u64,
    /// Rejections ignored: the delivery is settled or held by another lease.
    pub ignored: Vec<String>,
}

/// One change of the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteChange {
    /// The table.
    pub table: String,
    /// The primary key.
    pub key: Value,
    /// Upsert or delete.
    pub operation: Operation,
    /// The row, for an upsert; its primary key must be `key`.
    #[serde(default)]
    pub row: Option<Value>,
    /// The version the server holds after this change (opaque).
    pub server_version: Value,
    /// The local mutation this change is the echo of, if the server knows
    /// it: the change then acknowledges that mutation.
    #[serde(default)]
    pub mutation_id: Option<String>,
}

/// A page of server changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemotePage {
    /// The checkpoint the page was read after; it must be the current one.
    pub expected_checkpoint: Value,
    /// The checkpoint stored with the page.
    pub next_checkpoint: Value,
    /// The changes, in feed order.
    pub changes: Vec<RemoteChange>,
}

/// What [`SyncApi::apply_remote`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApplyOutcome {
    /// Changes written to their rows.
    pub applied: u64,
    /// Changes kept as conflicts (a local change was pending).
    pub conflicts: u64,
    /// Echoes that acknowledged a local mutation.
    pub acknowledged: u64,
    /// Changes already applied, or echoes of settled mutations.
    pub skipped: u64,
}

/// How a conflict ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    /// The server variant wins: the row takes it, and the local changes are
    /// settled as resolved (not acknowledged).
    AcceptRemote,
    /// The local row wins: it is sent again, on the server version.
    KeepLocal,
    /// `row` replaces both, and is sent on the server version.
    Merged {
        /// The merged row; its primary key must be the conflict's key.
        row: Value,
    },
}

/// The sync state of one row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityState {
    /// The summary.
    pub state: StateKind,
    /// The row is deleted (a tombstone, pending or settled).
    pub deleted: bool,
    /// The next change is leased right now.
    pub sending: bool,
    /// Times the next change was claimed.
    pub attempts: u64,
    /// The last error recorded for the next change.
    pub last_error: Option<String>,
    /// Changes not settled yet.
    pub pending: u64,
    /// Revision of the latest local change.
    pub local_revision: u64,
    /// Highest revision the server acknowledged.
    pub acknowledged_local_revision: u64,
    /// Every revision up to this one is acknowledged or resolved.
    pub settled_local_revision: u64,
    /// Changes whenever the local row changes, also by remote changes: the
    /// precondition of [`SyncApi::resolve_conflict`].
    pub row_version: u64,
    /// The last server version known for the row (opaque).
    pub server_version: Value,
    /// The open conflict, if any.
    pub conflict: Option<String>,
}

impl EntityState {
    /// The state of a row the engine holds no sync records of.
    fn without_records(state: StateKind) -> Self {
        Self {
            state,
            deleted: false,
            sending: false,
            attempts: 0,
            last_error: None,
            pending: 0,
            local_revision: 0,
            acknowledged_local_revision: 0,
            settled_local_revision: 0,
            row_version: 0,
            server_version: Value::Null,
            conflict: None,
        }
    }
}

/// One open change, as [`SyncApi::pending`] lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingChange {
    /// The mutation.
    pub mutation_id: String,
    /// The table.
    pub table: String,
    /// The primary key.
    pub key: Value,
    /// Its local revision.
    pub local_revision: u64,
    /// Upsert or delete.
    pub operation: Operation,
    /// Pending, leased or blocked.
    pub state: DeliveryKind,
    /// Times claimed.
    pub attempts: u64,
    /// The last error recorded.
    pub last_error: Option<String>,
}

/// Open changes of a remote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingChanges {
    /// How many changes of the remote are open (all tables).
    pub count: u64,
    /// The changes listed, in commit order.
    pub changes: Vec<PendingChange>,
}

/// A server change that met pending local changes (RFC §13.15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    /// Identity, for [`SyncApi::resolve_conflict`].
    pub id: String,
    /// The table.
    pub table: String,
    /// The primary key.
    pub key: Value,
    /// The current row version: the precondition to resolve it.
    pub local_row_version: u64,
    /// The local row now (`None` if deleted locally).
    pub local_row: Option<Value>,
    /// Open local mutations of the row.
    pub outstanding: Vec<String>,
    /// The server version of the remote variant.
    pub remote_version: Value,
    /// What the server did.
    pub remote_operation: Operation,
    /// The server row (`None` for a delete).
    pub remote_row: Option<Value>,
    /// The server version the local changes were based on.
    pub base_version: Value,
}

/// The sync status of a remote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteStatus {
    /// The checkpoint of the last applied page (`null` before the first).
    pub checkpoint: Value,
    /// Open changes.
    pub pending: u64,
    /// Open conflicts.
    pub conflicts: u64,
}

/// Sync records of one entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    generation: u64,
    local_revision: u64,
    acknowledged_local_revision: u64,
    settled_local_revision: u64,
    row_version: u64,
    server_version: Value,
    initialized: bool,
    deleted: bool,
    /// Sequences of the open changes, in order.
    open: Vec<u64>,
    last_mutation: Option<String>,
    conflict: Option<String>,
    last_acknowledged_at: Option<u64>,
}

impl Meta {
    fn new() -> Self {
        Self {
            generation: 1,
            local_revision: 0,
            acknowledged_local_revision: 0,
            settled_local_revision: 0,
            row_version: 0,
            server_version: Value::Null,
            initialized: false,
            deleted: false,
            open: Vec::new(),
            last_mutation: None,
            conflict: None,
            last_acknowledged_at: None,
        }
    }
}

/// An open change: the immutable mutation and its delivery state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Change {
    mutation_id: String,
    remote: String,
    table: String,
    key: Value,
    generation: u64,
    local_revision: u64,
    local_transaction_id: String,
    operation: Operation,
    row: Option<Value>,
    predecessor: Option<String>,
    state: DeliveryKind,
    attempts: u64,
    lease_id: Option<u64>,
    lease_expires_at: Option<u64>,
    /// Whether `base_version` is fixed (from the first claim on).
    prepared: bool,
    base_version: Value,
    last_error: Option<String>,
}

impl Change {
    fn envelope(&self) -> Envelope {
        Envelope {
            mutation_id: self.mutation_id.clone(),
            table: self.table.clone(),
            key: self.key.clone(),
            generation: self.generation,
            local_revision: self.local_revision,
            local_transaction_id: self.local_transaction_id.clone(),
            operation: self.operation,
            row: self.row.clone(),
            base_version: self.base_version.clone(),
            predecessor: self.predecessor.clone(),
            attempt: self.attempts,
        }
    }

    fn summary(&self, now: u64) -> PendingChange {
        PendingChange {
            mutation_id: self.mutation_id.clone(),
            table: self.table.clone(),
            key: self.key.clone(),
            local_revision: self.local_revision,
            operation: self.operation,
            state: self.delivery(now),
            attempts: self.attempts,
            last_error: self.last_error.clone(),
        }
    }

    /// The delivery state at `now`: an expired lease reads as pending.
    fn delivery(&self, now: u64) -> DeliveryKind {
        match self.state {
            DeliveryKind::Leased if !self.lease_alive(now) => DeliveryKind::Pending,
            state => state,
        }
    }

    fn lease_alive(&self, now: u64) -> bool {
        self.state == DeliveryKind::Leased && self.lease_expires_at.is_some_and(|end| end > now)
    }

    fn identifies(&self, table: &str, key: &Value, local_revision: u64) -> bool {
        self.table == table
            && encode_key(&self.key) == encode_key(key)
            && self.local_revision == local_revision
    }
}

/// Where a mutation is: open, or settled with a receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum MutationEntry {
    Open {
        sequence: u64,
    },
    Acknowledged {
        table: String,
        key: Value,
        local_revision: u64,
        server_version: Value,
    },
    Resolved {
        table: String,
        key: Value,
        local_revision: u64,
        reason: String,
    },
}

/// Per remote: the checkpoint and the counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RemoteRecord {
    checkpoint: Value,
    pending: u64,
    conflicts: u64,
}

/// The remote variant of a conflict.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConflictRecord {
    id: String,
    remote: String,
    table: String,
    key: Value,
    remote_version: Value,
    remote_operation: Operation,
    remote_row: Option<Value>,
    base_version: Value,
}

/// How a change was settled.
enum Settlement {
    Acknowledged(Value),
    Resolved(&'static str),
}

/// The sync operations of a database (see the [module](self)).
#[derive(Debug, Clone, Copy)]
pub struct SyncApi<'a> {
    db: &'a Db,
}

impl<'a> SyncApi<'a> {
    pub(crate) fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Leases the next eligible changes of `remote`: at most one per entity,
    /// in commit order, skipping entities with a conflict or a blocked
    /// change. Each envelope keeps its bytes on every claim.
    pub fn claim(&self, remote: &str, limits: &ClaimLimits) -> EngineResult<ClaimedBatch> {
        self.db.with_store(|store| claim(store, remote, limits))
    }

    /// Records the answer of the server to a push (see [`PushResult`]).
    /// Every acknowledgement is checked first; one that does not match its
    /// mutation fails the call without changing anything.
    pub fn apply_push_result(
        &self,
        remote: &str,
        result: &PushResult,
    ) -> EngineResult<PushOutcome> {
        self.db
            .with_store(|store| apply_push_result(store, remote, result))
    }

    /// Releases the deliveries still held by `lease_id` (pending again, with
    /// `reason` as their last error). Returns how many.
    pub fn release(&self, remote: &str, lease_id: u64, reason: &str) -> EngineResult<u64> {
        self.db
            .with_store(|store| release(store, remote, lease_id, reason))
    }

    /// Makes blocked mutations pending again. Returns how many were blocked.
    pub fn retry(&self, remote: &str, mutation_ids: &[String]) -> EngineResult<u64> {
        self.db
            .with_store(|store| retry(store, remote, mutation_ids))
    }

    /// Applies a page of server changes and stores its checkpoint, in one
    /// transaction (all or nothing). The changes do not produce local
    /// changes; one that meets a pending local change becomes a conflict.
    pub fn apply_remote(&self, remote: &str, page: &RemotePage) -> EngineResult<ApplyOutcome> {
        self.db
            .with_store(|store| apply_remote(store, remote, page))
    }

    /// Resolves the conflict `id`, if the row version is still
    /// `expected_row_version`. Refused while one of its changes is leased.
    pub fn resolve_conflict(
        &self,
        id: &str,
        expected_row_version: u64,
        resolution: &Resolution,
    ) -> EngineResult<()> {
        self.db
            .with_store(|store| resolve_conflict(store, id, expected_row_version, resolution))
    }

    /// The sync state of the row `key` of `table`; `None` when the row does
    /// not exist and the engine holds no record of it.
    pub fn state_of(&self, table: &str, key: &Value) -> EngineResult<Option<EntityState>> {
        self.db.with_store(|store| state_of(store, table, key))
    }

    /// The open changes of `remote`, in commit order, optionally of one
    /// table and at most `limit`.
    pub fn pending(
        &self,
        remote: &str,
        table: Option<&str>,
        limit: Option<u64>,
    ) -> EngineResult<PendingChanges> {
        self.db
            .with_store(|store| pending(store, remote, table, limit))
    }

    /// The open conflicts of `remote`.
    pub fn conflicts(&self, remote: &str) -> EngineResult<Vec<Conflict>> {
        self.db.with_store(|store| conflicts(store, remote))
    }

    /// The checkpoint and counters of `remote`.
    pub fn status(&self, remote: &str) -> EngineResult<RemoteStatus> {
        self.db.with_store(|store| status(store, remote))
    }
}

// --- Keys and records ---------------------------------------------------

fn with_name(tag: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(5 + name.len());
    key.push(tag);
    key.extend_from_slice(&(name.len() as u32).to_be_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

fn meta_key(table: &str, pk: &[u8]) -> Vec<u8> {
    let mut key = with_name(META, table);
    key.extend_from_slice(pk);
    key
}

fn change_key(sequence: u64) -> Vec<u8> {
    let mut key = vec![CHANGE];
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn open_key(remote: &str, sequence: u64) -> Vec<u8> {
    let mut key = with_name(OPEN, remote);
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn tagged(tag: u8, name: &str) -> Vec<u8> {
    let mut key = vec![tag];
    key.extend_from_slice(name.as_bytes());
    key
}

fn corrupt(detail: impl Into<String>) -> EngineError {
    EngineError::CorruptRecord {
        table: SYNC_DB_NAME.to_string(),
        detail: detail.into(),
    }
}

fn read<T: DeserializeOwned, Tx: Transaction>(
    txn: &Tx,
    db: Database,
    key: &[u8],
) -> EngineResult<Option<T>> {
    match txn.get(db, &key) {
        Ok(bytes) => serde_json::from_slice(bytes)
            .map(Some)
            .map_err(|e| corrupt(e.to_string())),
        Err(natdb::Error::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write<T: Serialize>(
    txn: &mut RwTransaction<'_>,
    db: Database,
    key: &[u8],
    value: &T,
) -> EngineResult<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| corrupt(e.to_string()))?;
    txn.put(db, &key, &bytes, WriteFlags::empty())?;
    Ok(())
}

fn remove(txn: &mut RwTransaction<'_>, db: Database, key: &[u8]) -> EngineResult<()> {
    match txn.del(db, &key, None) {
        Ok(()) | Err(natdb::Error::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Keys of `db` starting with `prefix`, in order.
fn keys_with_prefix<Tx: Transaction>(
    txn: &Tx,
    db: Database,
    prefix: &[u8],
) -> EngineResult<Vec<Vec<u8>>> {
    let mut cursor = txn.open_ro_cursor(db)?;
    let mut keys = Vec::new();
    for entry in cursor.iter_from(prefix) {
        let (key, _) = entry?;
        if !key.starts_with(prefix) {
            break;
        }
        keys.push(key.to_vec());
    }
    Ok(keys)
}

/// Sequences of the open changes of `remote`, in commit order.
fn open_sequences<Tx: Transaction>(txn: &Tx, db: Database, remote: &str) -> EngineResult<Vec<u64>> {
    let prefix = with_name(OPEN, remote);
    keys_with_prefix(txn, db, &prefix)?
        .into_iter()
        .map(|key| {
            key.get(prefix.len()..)
                .and_then(|tail| <[u8; 8]>::try_from(tail).ok())
                .map(u64::from_be_bytes)
                .ok_or_else(|| corrupt("open change key"))
        })
        .collect()
}

fn change<Tx: Transaction>(txn: &Tx, db: Database, sequence: u64) -> EngineResult<Change> {
    read(txn, db, &change_key(sequence))?.ok_or_else(|| corrupt(format!("change {sequence}")))
}

fn next_counter(txn: &mut RwTransaction<'_>, db: Database, name: &str) -> EngineResult<u64> {
    let key = tagged(COUNTER, name);
    let current: u64 = read(txn, db, &key)?.unwrap_or(0);
    let next = current + 1;
    write(txn, db, &key, &next)?;
    Ok(next)
}

/// The identity of this database among replicas, created once.
fn replica(txn: &mut RwTransaction<'_>, db: Database) -> EngineResult<String> {
    let key = tagged(COUNTER, "replica");
    if let Some(replica) = read::<String, _>(txn, db, &key)? {
        return Ok(replica);
    }
    // `RandomState` is seeded from the operating system's randomness.
    let random = |salt: u64| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u128(now_nanos());
        hasher.write_u32(process::id());
        hasher.finish()
    };
    let replica = format!("{:016x}{:016x}", random(1), random(2));
    write(txn, db, &key, &replica)?;
    Ok(replica)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

fn now_ms() -> u64 {
    u64::try_from(now_nanos() / 1_000_000).unwrap_or(u64::MAX)
}

fn remote_record<Tx: Transaction>(
    txn: &Tx,
    db: Database,
    remote: &str,
) -> EngineResult<RemoteRecord> {
    Ok(read(txn, db, &tagged(REMOTE, remote))?.unwrap_or_default())
}

fn update_remote(
    txn: &mut RwTransaction<'_>,
    db: Database,
    remote: &str,
    change: impl FnOnce(&mut RemoteRecord),
) -> EngineResult<()> {
    let mut record = remote_record(txn, db, remote)?;
    change(&mut record);
    write(txn, db, &tagged(REMOTE, remote), &record)
}

fn validate_remote(remote: &str) -> EngineResult<()> {
    if remote.is_empty() {
        return Err(EngineError::InvalidRequest(
            "the remote must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn sync_db(store: &Store) -> EngineResult<Database> {
    store
        .sync_db()
        .ok_or_else(|| corrupt("a synchronized table has no sync records"))
}

/// Runs `body` in a write transaction of its own, committed on `Ok`.
fn write_scope<R>(
    store: &Store,
    body: impl FnOnce(&mut RwTransaction<'_>, Database, u64) -> EngineResult<R>,
) -> EngineResult<R> {
    let db = sync_db(store)?;
    let _writer = WriterGuard::acquire(store)?;
    let mut txn = store.env().begin_rw_txn()?;
    let value = body(&mut txn, db, now_ms())?;
    txn.commit()?;
    Ok(value)
}

/// Runs `body` against a read snapshot.
fn read_scope<R>(
    store: &Store,
    body: impl FnOnce(&natdb::RoTransaction<'_>, Database, u64) -> EngineResult<R>,
) -> EngineResult<R> {
    let db = sync_db(store)?;
    if WriterGuard::active(store) {
        return Err(EngineError::Reentrancy);
    }
    let txn = store.env().begin_ro_txn()?;
    body(&txn, db, now_ms())
}

// --- Recording changes ----------------------------------------------------

/// Records the change of the row `pk` of `table` from `before` to `after`
/// in `txn`, the transaction of the write itself. Does nothing for a table
/// that is not synchronized, or when the row did not change.
pub(crate) fn track(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table: &Table,
    pk: &[u8],
    before: Option<&Value>,
    after: Option<&Value>,
) -> EngineResult<()> {
    let Some(remote) = table.def.sync.as_deref() else {
        return Ok(());
    };
    if before == after {
        return Ok(());
    }
    let db = sync_db(store)?;
    let key = after
        .or(before)
        .and_then(|row| field(row, &table.def.primary_key))
        .cloned()
        .ok_or_else(|| corrupt("a tracked row has no primary key"))?;
    let meta_key = meta_key(&table.def.name, pk);
    let mut meta: Meta = read(txn, db, &meta_key)?.unwrap_or_else(Meta::new);
    if before.is_none() && meta.deleted {
        if !meta.open.is_empty() {
            return Err(SyncError::TombstonePending {
                table: table.def.name.clone(),
                key: key.to_string(),
            }
            .into());
        }
        meta.generation += 1;
    }
    meta.row_version += 1;
    let operation = match after {
        Some(_) => Operation::Upsert,
        None => Operation::Delete,
    };
    record(
        txn,
        db,
        remote,
        &table.def.name,
        key,
        &mut meta,
        operation,
        after.cloned(),
    )?;
    write(txn, db, &meta_key, &meta)
}

/// Appends a change of the entity described by `meta` (written by the
/// caller). The changes of one root transaction share its LMDB transaction
/// id, which nested transactions inherit.
#[allow(clippy::too_many_arguments)]
fn record(
    txn: &mut RwTransaction<'_>,
    db: Database,
    remote: &str,
    table: &str,
    key: Value,
    meta: &mut Meta,
    operation: Operation,
    row: Option<Value>,
) -> EngineResult<()> {
    let sequence = next_counter(txn, db, "seq")?;
    let replica = replica(txn, db)?;
    let mutation_id = format!("{replica}-{sequence}");
    // SAFETY: `txn` is a live transaction; the call only reads its id.
    let transaction_id = unsafe { natdb_sys::mdb_txn_id(txn.txn()) };
    meta.local_revision += 1;
    meta.deleted = operation == Operation::Delete;
    meta.open.push(sequence);
    let predecessor = meta.last_mutation.replace(mutation_id.clone());
    let change = Change {
        mutation_id: mutation_id.clone(),
        remote: remote.to_string(),
        table: table.to_string(),
        key,
        generation: meta.generation,
        local_revision: meta.local_revision,
        local_transaction_id: format!("{replica}:{transaction_id}"),
        operation,
        row,
        predecessor,
        state: DeliveryKind::Pending,
        attempts: 0,
        lease_id: None,
        lease_expires_at: None,
        prepared: false,
        base_version: Value::Null,
        last_error: None,
    };
    write(txn, db, &change_key(sequence), &change)?;
    txn.put(db, &open_key(remote, sequence), b"", WriteFlags::empty())?;
    write(
        txn,
        db,
        &tagged(MUTATION, &mutation_id),
        &MutationEntry::Open { sequence },
    )?;
    update_remote(txn, db, remote, |record| record.pending += 1)
}

/// Closes the open change `sequence`, keeping a receipt of its mutation.
fn settle(
    txn: &mut RwTransaction<'_>,
    db: Database,
    sequence: u64,
    settlement: Settlement,
    now: u64,
) -> EngineResult<()> {
    let change = change(txn, db, sequence)?;
    remove(txn, db, &change_key(sequence))?;
    remove(txn, db, &open_key(&change.remote, sequence))?;
    let pk = encode_key(&change.key);
    let meta_key = meta_key(&change.table, &pk);
    let mut meta: Meta =
        read(txn, db, &meta_key)?.ok_or_else(|| corrupt("an open change has no entity"))?;
    meta.open.retain(|open| *open != sequence);
    let receipt = match settlement {
        Settlement::Acknowledged(server_version) => {
            meta.acknowledged_local_revision =
                meta.acknowledged_local_revision.max(change.local_revision);
            meta.server_version = server_version.clone();
            meta.initialized = true;
            meta.last_acknowledged_at = Some(now);
            MutationEntry::Acknowledged {
                table: change.table.clone(),
                key: change.key.clone(),
                local_revision: change.local_revision,
                server_version,
            }
        }
        Settlement::Resolved(reason) => MutationEntry::Resolved {
            table: change.table.clone(),
            key: change.key.clone(),
            local_revision: change.local_revision,
            reason: reason.to_string(),
        },
    };
    // Settled is the continuous prefix: an earlier open change holds it back.
    meta.settled_local_revision = match meta.open.first() {
        Some(&first) => self::change(txn, db, first)?.local_revision - 1,
        None => meta.local_revision,
    };
    write(txn, db, &meta_key, &meta)?;
    write(txn, db, &tagged(MUTATION, &change.mutation_id), &receipt)?;
    update_remote(txn, db, &change.remote, |record| {
        record.pending = record.pending.saturating_sub(1);
    })
}

/// Writes `after` as the row `pk` of `table` (or deletes it), keeping the
/// indexes in step, without recording a change.
fn write_row(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table: &Table,
    pk: &[u8],
    before: Option<&Value>,
    after: Option<&Value>,
) -> EngineResult<()> {
    if let Some(before) = before {
        exec::remove_index_entries(txn, table, before, pk)?;
    }
    match after {
        Some(row) => {
            exec::add_index_entries(store, txn, table, row, pk)?;
            txn.put(table.db, &pk, &exec::encode_row(row)?, WriteFlags::empty())?;
        }
        None if before.is_some() => txn.del(table.db, &pk, None)?,
        None => {}
    }
    Ok(())
}

fn current_row<Tx: Transaction>(txn: &Tx, table: &Table, pk: &[u8]) -> EngineResult<Option<Value>> {
    match txn.get(table.db, &pk) {
        Ok(bytes) => exec::decode_row(&table.def.name, bytes).map(Some),
        Err(natdb::Error::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Drops the sync records of `table` (its pending changes are discarded:
/// the table is gone). Called by [`Store::drop_table`].
pub(crate) fn purge_table(
    txn: &mut RwTransaction<'_>,
    db: Database,
    table: &str,
) -> EngineResult<()> {
    for key in keys_with_prefix(txn, db, &with_name(META, table))? {
        let Some(meta) = read::<Meta, _>(txn, db, &key)? else {
            continue;
        };
        for sequence in &meta.open {
            let change = change(txn, db, *sequence)?;
            remove(txn, db, &change_key(*sequence))?;
            remove(txn, db, &open_key(&change.remote, *sequence))?;
            remove(txn, db, &tagged(MUTATION, &change.mutation_id))?;
            update_remote(txn, db, &change.remote, |record| {
                record.pending = record.pending.saturating_sub(1);
            })?;
        }
        if let Some(id) = &meta.conflict {
            let conflict_key = tagged(CONFLICT, id);
            if let Some(conflict) = read::<ConflictRecord, _>(txn, db, &conflict_key)? {
                update_remote(txn, db, &conflict.remote, |record| {
                    record.conflicts = record.conflicts.saturating_sub(1);
                })?;
            }
            remove(txn, db, &conflict_key)?;
        }
        remove(txn, db, &key)?;
    }
    Ok(())
}

// --- Push -------------------------------------------------------------------

pub(crate) fn claim(
    store: &Store,
    remote: &str,
    limits: &ClaimLimits,
) -> EngineResult<ClaimedBatch> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(ClaimedBatch {
            lease_id: None,
            envelopes: Vec::new(),
        });
    }
    write_scope(store, |txn, db, now| {
        let mut seen = HashSet::new();
        let mut envelopes = Vec::new();
        let mut bytes = 0u64;
        let mut lease_id = None;
        for sequence in open_sequences(txn, db, remote)? {
            if envelopes.len() as u64 >= limits.max_changes {
                break;
            }
            let mut change = change(txn, db, sequence)?;
            let meta_key = meta_key(&change.table, &encode_key(&change.key));
            if !seen.insert(meta_key.clone()) {
                continue;
            }
            let meta: Meta =
                read(txn, db, &meta_key)?.ok_or_else(|| corrupt("an open change has no entity"))?;
            let eligible = meta.open.first() == Some(&sequence)
                && meta.conflict.is_none()
                && change.delivery(now) == DeliveryKind::Pending;
            if !eligible {
                continue;
            }
            if !change.prepared {
                change.prepared = true;
                change.base_version = meta.server_version.clone();
            }
            change.attempts += 1;
            let envelope = change.envelope();
            let size = serde_json::to_vec(&envelope).map_or(0, |json| json.len() as u64);
            if let Some(max) = limits.max_bytes {
                if !envelopes.is_empty() && bytes + size > max {
                    break;
                }
            }
            let lease = match lease_id {
                Some(lease) => lease,
                None => {
                    let lease = next_counter(txn, db, "lease")?;
                    lease_id = Some(lease);
                    lease
                }
            };
            change.state = DeliveryKind::Leased;
            change.lease_id = Some(lease);
            change.lease_expires_at = Some(now.saturating_add(limits.lease_ms));
            write(txn, db, &change_key(sequence), &change)?;
            envelopes.push(envelope);
            bytes += size;
        }
        Ok(ClaimedBatch {
            lease_id,
            envelopes,
        })
    })
}

/// Settles the mutation `ack` names; `false` for a duplicate.
fn acknowledge(
    txn: &mut RwTransaction<'_>,
    db: Database,
    remote: &str,
    ack: &Acknowledgement,
    now: u64,
) -> EngineResult<bool> {
    let mismatch = |detail: &str| -> EngineError {
        SyncError::AcknowledgementMismatch {
            mutation_id: ack.mutation_id.clone(),
            detail: detail.to_string(),
        }
        .into()
    };
    let same = |table: &str, key: &Value, local_revision: u64| {
        table == ack.table
            && encode_key(key) == encode_key(&ack.key)
            && local_revision == ack.local_revision
    };
    match read(txn, db, &tagged(MUTATION, &ack.mutation_id))? {
        None => Err(SyncError::UnknownMutation(ack.mutation_id.clone()).into()),
        Some(
            MutationEntry::Acknowledged {
                table,
                key,
                local_revision,
                ..
            }
            | MutationEntry::Resolved {
                table,
                key,
                local_revision,
                ..
            },
        ) => {
            if same(&table, &key, local_revision) {
                Ok(false)
            } else {
                Err(mismatch("another entity or revision"))
            }
        }
        Some(MutationEntry::Open { sequence }) => {
            let change = change(txn, db, sequence)?;
            if change.remote != remote {
                return Err(mismatch("the mutation belongs to another remote"));
            }
            if !change.identifies(&ack.table, &ack.key, ack.local_revision) {
                return Err(mismatch("another entity or revision"));
            }
            if change.attempts == 0 {
                return Err(mismatch("the mutation was never claimed"));
            }
            settle(
                txn,
                db,
                sequence,
                Settlement::Acknowledged(ack.server_version.clone()),
                now,
            )?;
            Ok(true)
        }
    }
}

/// Makes the deliveries held by `lease_id` pending again.
fn release_lease(
    txn: &mut RwTransaction<'_>,
    db: Database,
    remote: &str,
    lease_id: u64,
    reason: Option<&str>,
) -> EngineResult<u64> {
    let mut released = 0;
    for sequence in open_sequences(txn, db, remote)? {
        let mut change = change(txn, db, sequence)?;
        if change.state == DeliveryKind::Leased && change.lease_id == Some(lease_id) {
            change.state = DeliveryKind::Pending;
            change.lease_id = None;
            change.lease_expires_at = None;
            if let Some(reason) = reason {
                change.last_error = Some(reason.to_string());
            }
            write(txn, db, &change_key(sequence), &change)?;
            released += 1;
        }
    }
    Ok(released)
}

pub(crate) fn apply_push_result(
    store: &Store,
    remote: &str,
    result: &PushResult,
) -> EngineResult<PushOutcome> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return match result.acknowledged.first() {
            Some(ack) => Err(SyncError::UnknownMutation(ack.mutation_id.clone()).into()),
            None => Ok(PushOutcome::default()),
        };
    }
    write_scope(store, |txn, db, now| {
        let mut outcome = PushOutcome::default();
        for ack in &result.acknowledged {
            if acknowledge(txn, db, remote, ack, now)? {
                outcome.acknowledged += 1;
            }
        }
        for rejection in &result.rejected {
            let sequence = match read(txn, db, &tagged(MUTATION, &rejection.mutation_id))? {
                None => {
                    return Err(SyncError::UnknownMutation(rejection.mutation_id.clone()).into())
                }
                Some(MutationEntry::Open { sequence }) => sequence,
                Some(_) => {
                    outcome.ignored.push(rejection.mutation_id.clone());
                    continue;
                }
            };
            let mut change = change(txn, db, sequence)?;
            let held = change.state == DeliveryKind::Leased
                && result.lease_id.is_some()
                && change.lease_id == result.lease_id;
            if change.remote != remote || !held {
                outcome.ignored.push(rejection.mutation_id.clone());
                continue;
            }
            change.state = if rejection.retryable {
                DeliveryKind::Pending
            } else {
                DeliveryKind::Blocked
            };
            change.lease_id = None;
            change.lease_expires_at = None;
            change.last_error = Some(rejection.reason.clone());
            write(txn, db, &change_key(sequence), &change)?;
            outcome.rejected += 1;
        }
        if let Some(lease_id) = result.lease_id {
            outcome.released = release_lease(txn, db, remote, lease_id, None)?;
        }
        Ok(outcome)
    })
}

pub(crate) fn release(
    store: &Store,
    remote: &str,
    lease_id: u64,
    reason: &str,
) -> EngineResult<u64> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(0);
    }
    write_scope(store, |txn, db, _| {
        release_lease(txn, db, remote, lease_id, Some(reason))
    })
}

pub(crate) fn retry(store: &Store, remote: &str, mutation_ids: &[String]) -> EngineResult<u64> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(0);
    }
    write_scope(store, |txn, db, _| {
        let mut retried = 0;
        for mutation_id in mutation_ids {
            let Some(MutationEntry::Open { sequence }) =
                read(txn, db, &tagged(MUTATION, mutation_id))?
            else {
                continue;
            };
            let mut change = change(txn, db, sequence)?;
            if change.remote == remote && change.state == DeliveryKind::Blocked {
                change.state = DeliveryKind::Pending;
                write(txn, db, &change_key(sequence), &change)?;
                retried += 1;
            }
        }
        Ok(retried)
    })
}

// --- Pull and conflicts ---------------------------------------------------

pub(crate) fn apply_remote(
    store: &Store,
    remote: &str,
    page: &RemotePage,
) -> EngineResult<ApplyOutcome> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Err(SyncError::NotTracked(remote.to_string()).into());
    }
    write_scope(store, |txn, db, now| {
        let checkpoint = remote_record(txn, db, remote)?.checkpoint;
        if checkpoint != page.expected_checkpoint {
            return Err(SyncError::StaleCheckpoint {
                expected: page.expected_checkpoint.to_string(),
                actual: checkpoint.to_string(),
            }
            .into());
        }
        let mut outcome = ApplyOutcome::default();
        for change in &page.changes {
            apply_change(store, txn, db, remote, change, now, &mut outcome)?;
        }
        update_remote(txn, db, remote, |record| {
            record.checkpoint = page.next_checkpoint.clone();
        })?;
        Ok(outcome)
    })
}

fn apply_change(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    db: Database,
    remote: &str,
    change: &RemoteChange,
    now: u64,
    outcome: &mut ApplyOutcome,
) -> EngineResult<()> {
    let table = store.table(&change.table)?;
    if table.def.sync.as_deref() != Some(remote) {
        return Err(SyncError::NotTracked(change.table.clone()).into());
    }
    let pk = encode_key(&change.key);
    let row = match change.operation {
        Operation::Upsert => {
            let row = change
                .row
                .as_ref()
                .filter(|row| row.is_object())
                .ok_or_else(|| {
                    EngineError::InvalidRequest(format!(
                        "the upsert of {} in `{}` needs a row object",
                        change.key, change.table
                    ))
                })?;
            if table.primary_key(row).as_deref() != Some(pk.as_slice()) {
                return Err(EngineError::InvalidRequest(format!(
                    "the row of {} in `{}` holds another primary key",
                    change.key, change.table
                )));
            }
            Some(row)
        }
        Operation::Delete => None,
    };
    if let Some(mutation_id) = &change.mutation_id {
        match read(txn, db, &tagged(MUTATION, mutation_id))? {
            Some(MutationEntry::Open { sequence }) => {
                let local = self::change(txn, db, sequence)?;
                if local.table == change.table && encode_key(&local.key) == pk && local.attempts > 0
                {
                    settle(
                        txn,
                        db,
                        sequence,
                        Settlement::Acknowledged(change.server_version.clone()),
                        now,
                    )?;
                    outcome.acknowledged += 1;
                    return Ok(());
                }
            }
            Some(_) => {
                outcome.skipped += 1;
                return Ok(());
            }
            None => {}
        }
    }
    let meta_key = meta_key(&change.table, &pk);
    let mut meta: Meta = read(txn, db, &meta_key)?.unwrap_or_else(Meta::new);
    let quiet = meta.open.is_empty() && meta.conflict.is_none();
    if quiet && meta.initialized && meta.server_version == change.server_version {
        outcome.skipped += 1;
        return Ok(());
    }
    if !quiet {
        if let Some(id) = &meta.conflict {
            let existing: Option<ConflictRecord> = read(txn, db, &tagged(CONFLICT, id))?;
            if existing.is_some_and(|c| c.remote_version == change.server_version) {
                outcome.skipped += 1;
                return Ok(());
            }
        }
        let id = match &meta.conflict {
            Some(id) => id.clone(),
            None => {
                let id = format!("conflict-{}", next_counter(txn, db, "conflict")?);
                update_remote(txn, db, remote, |record| record.conflicts += 1)?;
                id
            }
        };
        let conflict = ConflictRecord {
            id: id.clone(),
            remote: remote.to_string(),
            table: change.table.clone(),
            key: change.key.clone(),
            remote_version: change.server_version.clone(),
            remote_operation: change.operation,
            remote_row: row.cloned(),
            base_version: meta.server_version.clone(),
        };
        write(txn, db, &tagged(CONFLICT, &id), &conflict)?;
        meta.conflict = Some(id);
        write(txn, db, &meta_key, &meta)?;
        outcome.conflicts += 1;
        return Ok(());
    }
    let before = current_row(txn, &table, &pk)?;
    write_row(store, txn, &table, &pk, before.as_ref(), row)?;
    if row.is_some() && meta.deleted {
        meta.generation += 1;
    }
    meta.server_version = change.server_version.clone();
    meta.initialized = true;
    meta.deleted = row.is_none();
    meta.row_version += 1;
    write(txn, db, &meta_key, &meta)?;
    outcome.applied += 1;
    Ok(())
}

pub(crate) fn resolve_conflict(
    store: &Store,
    id: &str,
    expected_row_version: u64,
    resolution: &Resolution,
) -> EngineResult<()> {
    if store.sync_db().is_none() {
        return Err(SyncError::ConflictNotFound(id.to_string()).into());
    }
    write_scope(store, |txn, db, now| {
        let conflict_key = tagged(CONFLICT, id);
        let conflict: ConflictRecord = read(txn, db, &conflict_key)?
            .ok_or_else(|| SyncError::ConflictNotFound(id.to_string()))?;
        let table = store.table(&conflict.table)?;
        let pk = encode_key(&conflict.key);
        let meta_key = meta_key(&conflict.table, &pk);
        let meta: Meta =
            read(txn, db, &meta_key)?.ok_or_else(|| corrupt("a conflict has no entity"))?;
        if meta.row_version != expected_row_version {
            return Err(SyncError::RowVersionMismatch {
                expected: expected_row_version,
                actual: meta.row_version,
            }
            .into());
        }
        for sequence in &meta.open {
            let change = change(txn, db, *sequence)?;
            if change.lease_alive(now) {
                return Err(SyncError::MutationInFlight(change.mutation_id).into());
            }
        }
        let merged = match resolution {
            Resolution::Merged { row } => {
                if !row.is_object() || table.primary_key(row).as_deref() != Some(pk.as_slice()) {
                    return Err(EngineError::InvalidRequest(format!(
                        "the merged row of {} in `{}` must be an object with that primary key",
                        conflict.key, conflict.table
                    )));
                }
                Some(row)
            }
            Resolution::AcceptRemote | Resolution::KeepLocal => None,
        };
        let reason = match resolution {
            Resolution::AcceptRemote => "accept_remote",
            Resolution::KeepLocal => "keep_local",
            Resolution::Merged { .. } => "merged",
        };
        for sequence in meta.open.clone() {
            settle(txn, db, sequence, Settlement::Resolved(reason), now)?;
        }
        remove(txn, db, &conflict_key)?;
        update_remote(txn, db, &conflict.remote, |record| {
            record.conflicts = record.conflicts.saturating_sub(1);
        })?;
        let mut meta: Meta =
            read(txn, db, &meta_key)?.ok_or_else(|| corrupt("a conflict has no entity"))?;
        meta.server_version = conflict.remote_version.clone();
        meta.initialized = true;
        meta.conflict = None;
        let current = current_row(txn, &table, &pk)?;
        let resend = match resolution {
            Resolution::AcceptRemote => {
                let remote_row = conflict.remote_row.as_ref();
                write_row(store, txn, &table, &pk, current.as_ref(), remote_row)?;
                meta.deleted = remote_row.is_none();
                meta.row_version += 1;
                None
            }
            Resolution::KeepLocal => match (&current, conflict.remote_operation) {
                (None, Operation::Delete) => None,
                (Some(row), _) => Some((Operation::Upsert, Some(row.clone()))),
                (None, Operation::Upsert) => Some((Operation::Delete, None)),
            },
            Resolution::Merged { .. } => {
                write_row(store, txn, &table, &pk, current.as_ref(), merged)?;
                meta.row_version += 1;
                Some((Operation::Upsert, merged.cloned()))
            }
        };
        if let Some((operation, row)) = resend {
            record(
                txn,
                db,
                &conflict.remote,
                &conflict.table,
                conflict.key.clone(),
                &mut meta,
                operation,
                row,
            )?;
        }
        write(txn, db, &meta_key, &meta)
    })
}

// --- State ----------------------------------------------------------------

pub(crate) fn state_of(
    store: &Store,
    table_name: &str,
    key: &Value,
) -> EngineResult<Option<EntityState>> {
    let table = store.table(table_name)?;
    if table.def.sync.is_none() {
        return Ok(Some(EntityState::without_records(StateKind::LocalOnly)));
    }
    let pk = encode_key(key);
    read_scope(store, |txn, db, now| {
        let Some(meta) = read::<Meta, _>(txn, db, &meta_key(table_name, &pk))? else {
            return Ok(current_row(txn, &table, &pk)?
                .map(|_| EntityState::without_records(StateKind::Unknown)));
        };
        let next = match meta.open.first() {
            Some(&sequence) => Some(change(txn, db, sequence)?),
            None => None,
        };
        let state = if meta.conflict.is_some() {
            StateKind::Conflict
        } else if next
            .as_ref()
            .is_some_and(|c| c.state == DeliveryKind::Blocked)
        {
            StateKind::Blocked
        } else if next.is_some() {
            StateKind::Pending
        } else if meta.initialized {
            StateKind::Synced
        } else {
            StateKind::Unknown
        };
        Ok(Some(EntityState {
            state,
            deleted: meta.deleted,
            sending: next.as_ref().is_some_and(|c| c.lease_alive(now)),
            attempts: next.as_ref().map_or(0, |c| c.attempts),
            last_error: next.and_then(|c| c.last_error),
            pending: meta.open.len() as u64,
            local_revision: meta.local_revision,
            acknowledged_local_revision: meta.acknowledged_local_revision,
            settled_local_revision: meta.settled_local_revision,
            row_version: meta.row_version,
            server_version: meta.server_version,
            conflict: meta.conflict,
        }))
    })
}

pub(crate) fn pending(
    store: &Store,
    remote: &str,
    table: Option<&str>,
    limit: Option<u64>,
) -> EngineResult<PendingChanges> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(PendingChanges {
            count: 0,
            changes: Vec::new(),
        });
    }
    read_scope(store, |txn, db, now| {
        let count = remote_record(txn, db, remote)?.pending;
        let limit = limit
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(usize::MAX);
        let mut changes = Vec::new();
        for sequence in open_sequences(txn, db, remote)? {
            if changes.len() >= limit {
                break;
            }
            let change = change(txn, db, sequence)?;
            if table.is_none_or(|name| name == change.table) {
                changes.push(change.summary(now));
            }
        }
        Ok(PendingChanges { count, changes })
    })
}

pub(crate) fn conflicts(store: &Store, remote: &str) -> EngineResult<Vec<Conflict>> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(Vec::new());
    }
    read_scope(store, |txn, db, _| {
        let mut conflicts = Vec::new();
        for key in keys_with_prefix(txn, db, &[CONFLICT])? {
            let Some(conflict) = read::<ConflictRecord, _>(txn, db, &key)? else {
                continue;
            };
            if conflict.remote != remote {
                continue;
            }
            let table = store.table(&conflict.table)?;
            let pk = encode_key(&conflict.key);
            let meta: Meta = read(txn, db, &meta_key(&conflict.table, &pk))?
                .ok_or_else(|| corrupt("a conflict has no entity"))?;
            let outstanding = meta
                .open
                .iter()
                .map(|sequence| change(txn, db, *sequence).map(|c| c.mutation_id))
                .collect::<EngineResult<Vec<_>>>()?;
            conflicts.push(Conflict {
                id: conflict.id,
                table: conflict.table,
                key: conflict.key,
                local_row_version: meta.row_version,
                local_row: current_row(txn, &table, &pk)?,
                outstanding,
                remote_version: conflict.remote_version,
                remote_operation: conflict.remote_operation,
                remote_row: conflict.remote_row,
                base_version: conflict.base_version,
            });
        }
        Ok(conflicts)
    })
}

pub(crate) fn status(store: &Store, remote: &str) -> EngineResult<RemoteStatus> {
    validate_remote(remote)?;
    if store.sync_db().is_none() {
        return Ok(RemoteStatus {
            checkpoint: Value::Null,
            pending: 0,
            conflicts: 0,
        });
    }
    read_scope(store, |txn, db, _| {
        let record = remote_record(txn, db, remote)?;
        Ok(RemoteStatus {
            checkpoint: record.checkpoint,
            pending: record.pending,
            conflicts: record.conflicts,
        })
    })
}
