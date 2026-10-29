//! Persistence: the operation log and the append-only journal.
//!
//! - `ops`: one row per operation, written before any call to an operator and
//!   updated as its status changes.
//! - `journal`: one row per finished operation that moved money, never
//!   updated or deleted. The sum of a rail's entries must equal its balance
//!   to the sat (test 1.10).
//! - `intents`: one row per move between pockets, saved before each step.

use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use indigo_domain::{Error, IntentState, Msat, OpId, OpKind, OpStatus, RailId, Result, now};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpRecord {
    pub op: OpId,
    pub rail: RailId,
    pub kind: OpKind,
    pub amount: Msat,
    pub status: OpStatus,
    /// The invoice, or a note; never a token or anything secret.
    pub detail: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    pub seq: i64,
    pub op: OpId,
    pub rail: RailId,
    /// Signed msat: + into the wallet, - out of it (fees included).
    pub delta: i64,
    pub fee: Msat,
    pub at: u64,
}

/// A move between pockets (see `indigo-engine`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentRecord {
    pub id: String,
    /// The caller's idempotency key: a key already used returns that move
    /// instead of starting a second one.
    pub key: Option<String>,
    pub from: RailId,
    pub to: RailId,
    pub amount: Msat,
    pub state: IntentState,
    /// The destination's invoice and its receive operation.
    pub invoice: Option<String>,
    pub dest_op: Option<OpId>,
    /// The source's quote for paying the invoice.
    pub quote_id: Option<String>,
    pub fee_reserve: Option<Msat>,
    pub quote_expires_at: Option<u64>,
    pub source_op: Option<OpId>,
    /// Why it failed or needs attention.
    pub note: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ops (
    op          TEXT PRIMARY KEY,
    rail        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    amount      INTEGER NOT NULL,
    status      TEXT NOT NULL,
    fee         INTEGER,
    reason      TEXT,
    detail      TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS intents (
    id                TEXT PRIMARY KEY,
    key               TEXT UNIQUE,
    from_rail         TEXT NOT NULL,
    to_rail           TEXT NOT NULL,
    amount            INTEGER NOT NULL,
    state             TEXT NOT NULL,
    invoice           TEXT,
    dest_op           TEXT,
    quote_id          TEXT,
    fee_reserve       INTEGER,
    quote_expires_at  INTEGER,
    source_op         TEXT,
    note              TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journal (
    seq    INTEGER PRIMARY KEY AUTOINCREMENT,
    op     TEXT NOT NULL UNIQUE REFERENCES ops(op),
    rail   TEXT NOT NULL,
    delta  INTEGER NOT NULL,
    fee    INTEGER NOT NULL,
    at     INTEGER NOT NULL
);
";

/// Version 1 counts amounts in msat; version 0 (Phase 1) counted whole sats.
const VERSION: i64 = 1;

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(db)?;
    let existed: bool = conn
        .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'ops')", [], |r| r.get(0))
        .map_err(db)?;
    let sats_to_msat = if existed && version == 0 {
        "UPDATE ops SET amount = amount * 1000, fee = fee * 1000;
         UPDATE journal SET delta = delta * 1000, fee = fee * 1000;"
    } else {
        ""
    };
    conn.execute_batch(&format!("BEGIN; {SCHEMA} {sats_to_msat} PRAGMA user_version = {VERSION}; COMMIT;"))
        .map_err(db)
}

fn db(e: rusqlite::Error) -> Error {
    Error::Storage(e.to_string())
}

impl Store {
    /// Opens (or creates) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(db)?;
        // WAL keeps a crash mid-write from corrupting the file.
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db)?;
        migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records an operation as pending, then moves it to `op.status` if that
    /// is final. Recording the same op twice is a no-op, so a retried step
    /// can't create a duplicate; a retry of a failed op (nothing moved) opens
    /// it again.
    pub fn record(&self, op: &OpRecord) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO ops (op, rail, kind, amount, status, detail, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'PENDING', ?5, ?6, ?6)
                 ON CONFLICT (op) DO UPDATE SET status = 'PENDING', fee = NULL, reason = NULL, updated_at = ?6
                 WHERE ops.status = 'FAILED'",
                params![op.op.0, op.rail.0, op.kind.as_str(), op.amount.0 as i64, op.detail, op.created_at as i64],
            )
            .map_err(db)?;
        if op.status.is_final() {
            self.set_status(&op.op, &op.status)?;
        }
        Ok(())
    }

    /// Moves a pending operation to a final `status`. A final status never
    /// changes again. When it becomes `Completed`, its journal entry is
    /// written in the same transaction, so a crash can never leave one without
    /// the other. Returns whether anything changed.
    pub fn set_status(&self, op: &OpId, status: &OpStatus) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let row: Option<(String, String, i64, Option<i64>)> = tx
            .query_row("SELECT rail, kind, amount, fee FROM ops WHERE op = ?1", [&op.0], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()
            .map_err(db)?;
        let Some((rail, kind, _amount, _)) = row else {
            return Err(Error::UnknownOp(op.clone()));
        };
        if !status.is_final() {
            return Ok(false);
        }
        let (label, fee, reason) = encode(status);
        let changed = tx
            .execute(
                "UPDATE ops SET status = ?2, fee = ?3, reason = ?4, updated_at = ?5 WHERE op = ?1 AND status = 'PENDING'",
                params![op.0, label, fee, reason, now() as i64],
            )
            .map_err(db)?
            > 0;
        let entry = match status {
            OpStatus::Completed { amount, fee } => Some(match OpKind::from_str(&kind)? {
                OpKind::LnReceive | OpKind::NativeReceive => (amount.0 as i64 - fee.0 as i64, *fee),
                OpKind::LnPay | OpKind::NativeSend => (-(amount.0 as i64 + fee.0 as i64), *fee),
            }),
            // A failed attempt that still cost something (fees on a refund).
            OpStatus::Failed { cost, .. } if *cost > Msat::ZERO => Some((-(cost.0 as i64), *cost)),
            _ => None,
        };
        if let (true, Some((delta, fee))) = (changed, entry) {
            tx.execute(
                "INSERT OR IGNORE INTO journal (op, rail, delta, fee, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![op.0, rail, delta, fee.0 as i64, now() as i64],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(changed)
    }

    /// Corrects an operation recorded as expired, or as failed at no cost,
    /// to `Completed` when its operator later reports that it went through.
    /// Nothing was journaled for it before, so the entry is written now.
    /// Returns whether anything changed.
    pub fn correct_to_completed(&self, op: &OpId, amount: Msat, fee: Msat) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db)?;
        let changed = tx
            .execute(
                "UPDATE ops SET status = 'PENDING', fee = NULL, reason = NULL
                 WHERE op = ?1 AND (status = 'EXPIRED' OR (status = 'FAILED' AND COALESCE(fee, 0) = 0))",
                [&op.0],
            )
            .map_err(db)?
            > 0;
        tx.commit().map_err(db)?;
        drop(conn);
        if !changed {
            return Ok(false);
        }
        self.set_status(op, &OpStatus::Completed { amount, fee })
    }

    pub fn get(&self, op: &OpId) -> Result<Option<OpRecord>> {
        self.conn()
            .query_row(&format!("{SELECT_OPS} WHERE op = ?1"), [&op.0], read_op)
            .optional()
            .map_err(db)
    }

    /// All operations, newest first.
    pub fn ops(&self) -> Result<Vec<OpRecord>> {
        self.query_ops(&format!("{SELECT_OPS} ORDER BY created_at DESC, op"))
    }

    /// Operations that are not final yet: what `recover()` must look at.
    pub fn open_ops(&self) -> Result<Vec<OpRecord>> {
        self.query_ops(&format!("{SELECT_OPS} WHERE status = 'PENDING' ORDER BY created_at"))
    }

    fn query_ops(&self, sql: &str) -> Result<Vec<OpRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).map_err(db)?;
        let rows = stmt.query_map([], read_op).map_err(db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db)
    }

    pub fn journal(&self) -> Result<Vec<JournalEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT seq, op, rail, delta, fee, at FROM journal ORDER BY seq").map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(JournalEntry {
                    seq: r.get(0)?,
                    op: OpId(r.get(1)?),
                    rail: RailId(r.get(2)?),
                    delta: r.get(3)?,
                    fee: Msat(r.get::<_, i64>(4)? as u64),
                    at: r.get::<_, i64>(5)? as u64,
                })
            })
            .map_err(db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db)
    }

    /// Saves a new move, or returns the existing one if its key was used before.
    pub fn create_intent(&self, intent: &IntentRecord) -> Result<IntentRecord> {
        self.conn()
            .execute(
                "INSERT INTO intents (id, key, from_rail, to_rail, amount, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) ON CONFLICT (key) DO NOTHING",
                params![
                    intent.id,
                    intent.key,
                    intent.from.0,
                    intent.to.0,
                    intent.amount.0 as i64,
                    intent.state.as_str(),
                    intent.created_at as i64
                ],
            )
            .map_err(db)?;
        let found = match &intent.key {
            Some(key) => self.query_intents(&format!("{SELECT_INTENTS} WHERE key = ?1"), [key])?,
            None => self.query_intents(&format!("{SELECT_INTENTS} WHERE id = ?1"), [&intent.id])?,
        };
        found.into_iter().next().ok_or_else(|| Error::Storage(format!("move {} was not saved", intent.id)))
    }

    /// Saves `intent` only if it is still in state `from`, so two processes
    /// can never both advance the same move. Returns whether it was saved.
    pub fn save_intent(&self, intent: &IntentRecord, from: IntentState) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                "UPDATE intents SET state = ?2, invoice = ?3, dest_op = ?4, quote_id = ?5, fee_reserve = ?6,
                     quote_expires_at = ?7, source_op = ?8, note = ?9, updated_at = ?10
                 WHERE id = ?1 AND state = ?11",
                params![
                    intent.id,
                    intent.state.as_str(),
                    intent.invoice,
                    intent.dest_op.as_ref().map(|o| &o.0),
                    intent.quote_id,
                    intent.fee_reserve.map(|f| f.0 as i64),
                    intent.quote_expires_at.map(|t| t as i64),
                    intent.source_op.as_ref().map(|o| &o.0),
                    intent.note,
                    now() as i64,
                    from.as_str()
                ],
            )
            .map_err(db)?;
        Ok(changed > 0)
    }

    pub fn intent(&self, id: &str) -> Result<Option<IntentRecord>> {
        Ok(self.query_intents(&format!("{SELECT_INTENTS} WHERE id = ?1"), [id])?.into_iter().next())
    }

    /// All moves, newest first.
    pub fn intents(&self) -> Result<Vec<IntentRecord>> {
        self.query_intents(&format!("{SELECT_INTENTS} ORDER BY created_at DESC, id DESC"), [])
    }

    /// Moves not final yet: what recovery must look at.
    pub fn open_intents(&self) -> Result<Vec<IntentRecord>> {
        self.query_intents(
            &format!("{SELECT_INTENTS} WHERE state NOT IN ('COMPLETED', 'FAILED', 'EXPIRED') ORDER BY created_at"),
            [],
        )
    }

    fn query_intents<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<Vec<IntentRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql).map_err(db)?;
        let rows = stmt.query_map(params, read_intent).map_err(db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db)
    }

    /// The sum of a rail's journal entries: what its balance must be.
    pub fn journal_total(&self, rail: &RailId) -> Result<i64> {
        self.conn()
            .query_row("SELECT COALESCE(SUM(delta), 0) FROM journal WHERE rail = ?1", [&rail.0], |r| r.get(0))
            .map_err(db)
    }
}

const SELECT_INTENTS: &str = "SELECT id, key, from_rail, to_rail, amount, state, invoice, dest_op, quote_id,
    fee_reserve, quote_expires_at, source_op, note, created_at, updated_at FROM intents";

fn read_intent(r: &rusqlite::Row<'_>) -> rusqlite::Result<IntentRecord> {
    let state: String = r.get(5)?;
    Ok(IntentRecord {
        id: r.get(0)?,
        key: r.get(1)?,
        from: RailId(r.get(2)?),
        to: RailId(r.get(3)?),
        amount: Msat(r.get::<_, i64>(4)? as u64),
        state: IntentState::from_str(&state).map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        invoice: r.get(6)?,
        dest_op: r.get::<_, Option<String>>(7)?.map(OpId),
        quote_id: r.get(8)?,
        fee_reserve: r.get::<_, Option<i64>>(9)?.map(|f| Msat(f as u64)),
        quote_expires_at: r.get::<_, Option<i64>>(10)?.map(|t| t as u64),
        source_op: r.get::<_, Option<String>>(11)?.map(OpId),
        note: r.get(12)?,
        created_at: r.get::<_, i64>(13)? as u64,
        updated_at: r.get::<_, i64>(14)? as u64,
    })
}

const SELECT_OPS: &str =
    "SELECT op, rail, kind, amount, status, fee, reason, detail, created_at, updated_at FROM ops";

fn read_op(r: &rusqlite::Row<'_>) -> rusqlite::Result<OpRecord> {
    let kind: String = r.get(2)?;
    let amount = Msat(r.get::<_, i64>(3)? as u64);
    let status: String = r.get(4)?;
    let fee: Option<i64> = r.get(5)?;
    let reason: Option<String> = r.get(6)?;
    let status = match status.as_str() {
        "COMPLETED" => OpStatus::Completed { amount, fee: Msat(fee.unwrap_or(0) as u64) },
        "FAILED" => OpStatus::Failed { reason: reason.unwrap_or_default(), cost: Msat(fee.unwrap_or(0) as u64) },
        "EXPIRED" => OpStatus::Expired,
        _ => OpStatus::Pending,
    };
    Ok(OpRecord {
        op: OpId(r.get(0)?),
        rail: RailId(r.get(1)?),
        kind: OpKind::from_str(&kind).map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        amount,
        status,
        detail: r.get(7)?,
        created_at: r.get::<_, i64>(8)? as u64,
        updated_at: r.get::<_, i64>(9)? as u64,
    })
}

fn encode(status: &OpStatus) -> (&'static str, Option<i64>, Option<String>) {
    match status {
        OpStatus::Completed { fee, .. } => (status.label(), Some(fee.0 as i64), None),
        // A failed op's `fee` column holds what the attempt still cost.
        OpStatus::Failed { reason, cost } => (status.label(), Some(cost.0 as i64), Some(reason.clone())),
        _ => (status.label(), None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(Path::new(":memory:")).unwrap()
    }

    fn pending(op: &str, kind: OpKind, amount: u64) -> OpRecord {
        OpRecord {
            op: OpId(op.into()),
            rail: RailId("cashu:test".into()),
            kind,
            amount: Msat(amount),
            status: OpStatus::Pending,
            detail: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn journal_written_once_per_completed_op() {
        let s = store();
        s.record(&pending("r1", OpKind::LnReceive, 10_000)).unwrap();
        let done = OpStatus::Completed { amount: Msat(10_000), fee: Msat::ZERO };
        assert!(s.set_status(&OpId("r1".into()), &done).unwrap());
        // Reporting the same final status again (e.g. from recover) adds nothing.
        s.set_status(&OpId("r1".into()), &done).unwrap();
        assert_eq!(s.journal().unwrap().len(), 1);
        assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), 10_000);
    }

    #[test]
    fn payments_debit_amount_plus_fee() {
        let s = store();
        s.record(&pending("p1", OpKind::LnPay, 3_000)).unwrap();
        s.set_status(&OpId("p1".into()), &OpStatus::Completed { amount: Msat(3_000), fee: Msat(12) }).unwrap();
        assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), -3_012);
    }

    #[test]
    fn final_status_never_changes_and_never_journals() {
        let s = store();
        s.record(&pending("p2", OpKind::LnPay, 500)).unwrap();
        s.set_status(&OpId("p2".into()), &OpStatus::failed("no route")).unwrap();
        let changed = s
            .set_status(&OpId("p2".into()), &OpStatus::Completed { amount: Msat(500), fee: Msat::ZERO })
            .unwrap();
        assert!(!changed);
        assert!(matches!(s.get(&OpId("p2".into())).unwrap().unwrap().status, OpStatus::Failed { .. }));
        assert!(s.journal().unwrap().is_empty());
    }

    #[test]
    fn recording_twice_is_a_no_op() {
        let s = store();
        s.record(&pending("r2", OpKind::LnReceive, 1)).unwrap();
        s.record(&pending("r2", OpKind::LnReceive, 1)).unwrap();
        assert_eq!(s.ops().unwrap().len(), 1);
        assert_eq!(s.open_ops().unwrap().len(), 1);
    }

    #[test]
    fn a_failed_attempt_that_cost_fees_is_journaled() {
        let s = store();
        s.record(&pending("p4", OpKind::LnPay, 300)).unwrap();
        let refunded = OpStatus::Failed { reason: "refunded".into(), cost: Msat(512) };
        s.set_status(&OpId("p4".into()), &refunded).unwrap();
        assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), -512);
        assert_eq!(s.get(&OpId("p4".into())).unwrap().unwrap().status, refunded);
    }

    #[test]
    fn retrying_a_failed_op_reopens_it() {
        let s = store();
        s.record(&pending("p3", OpKind::LnPay, 500)).unwrap();
        s.set_status(&OpId("p3".into()), &OpStatus::failed("mint said no")).unwrap();
        s.record(&pending("p3", OpKind::LnPay, 500)).unwrap();
        assert_eq!(s.open_ops().unwrap().len(), 1);
        // A completed op is not reopened.
        let done = OpStatus::Completed { amount: Msat(500), fee: Msat(1) };
        s.set_status(&OpId("p3".into()), &done).unwrap();
        s.record(&pending("p3", OpKind::LnPay, 500)).unwrap();
        assert_eq!(s.get(&OpId("p3".into())).unwrap().unwrap().status, done);
        assert_eq!(s.journal().unwrap().len(), 1);
    }

    #[test]
    fn recording_a_completed_op_journals_it() {
        let s = store();
        let mut op = pending("t1", OpKind::NativeReceive, 2_000);
        op.status = OpStatus::Completed { amount: Msat(2_000), fee: Msat(2) };
        s.record(&op).unwrap();
        assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), 1_998);
    }

    #[test]
    fn phase_1_store_moves_to_msat() {
        let path = std::env::temp_dir().join(format!("indigo-migrate-{}.sqlite", std::process::id()));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO ops VALUES ('r', 'cashu:test', 'ln_receive', 10, 'COMPLETED', 1, NULL, NULL, 1, 1);
                 INSERT INTO journal (op, rail, delta, fee, at) VALUES ('r', 'cashu:test', 9, 1, 1);",
            )
            .unwrap();
        }
        for _ in 0..2 {
            // Opening twice converts once.
            let s = Store::open(&path).unwrap();
            assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), 9_000);
            assert_eq!(s.get(&OpId("r".into())).unwrap().unwrap().amount, Msat::from_sats(10));
        }
        let _ = std::fs::remove_file(&path);
    }

    fn intent(id: &str, key: Option<&str>) -> IntentRecord {
        IntentRecord {
            id: id.into(),
            key: key.map(Into::into),
            from: RailId("cashu:test".into()),
            to: RailId("fedimint:test".into()),
            amount: Msat::from_sats(5_000),
            state: IntentState::Quoted,
            invoice: None,
            dest_op: None,
            quote_id: None,
            fee_reserve: None,
            quote_expires_at: None,
            source_op: None,
            note: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn a_used_key_returns_the_first_move() {
        let s = store();
        let first = s.create_intent(&intent("m1", Some("k"))).unwrap();
        let again = s.create_intent(&intent("m2", Some("k"))).unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(s.intents().unwrap().len(), 1);
    }

    #[test]
    fn a_move_advances_only_from_the_state_it_was_read_in() {
        let s = store();
        let mut m = s.create_intent(&intent("m1", None)).unwrap();
        m.state = IntentState::DestInvoiceCreated;
        assert!(s.save_intent(&m, IntentState::Quoted).unwrap());
        // A second process that read it as QUOTED can't move it again.
        m.state = IntentState::Failed;
        assert!(!s.save_intent(&m, IntentState::Quoted).unwrap());
        assert_eq!(s.intent("m1").unwrap().unwrap().state, IntentState::DestInvoiceCreated);
        assert_eq!(s.open_intents().unwrap().len(), 1);
    }

    #[test]
    fn an_expired_op_reported_completed_later_is_corrected_and_journaled() {
        let s = store();
        s.record(&pending("r3", OpKind::LnReceive, 5_000)).unwrap();
        s.set_status(&OpId("r3".into()), &OpStatus::Expired).unwrap();
        assert!(s.correct_to_completed(&OpId("r3".into()), Msat(5_000), Msat(17)).unwrap());
        assert_eq!(s.journal_total(&RailId("cashu:test".into())).unwrap(), 4_983);
        // A completed op is never corrected again.
        assert!(!s.correct_to_completed(&OpId("r3".into()), Msat(5_000), Msat(17)).unwrap());
    }

    #[test]
    fn unknown_op_is_an_error() {
        assert!(matches!(store().set_status(&OpId("nope".into()), &OpStatus::Expired), Err(Error::UnknownOp(_))));
    }
}
