//! Moves money between two pockets: the destination issues a Lightning
//! invoice and the source pays it.
//!
//! Every state is saved before the next call to an operator, and each save is
//! a compare-and-set on the state it was read in, so a crash at any point
//! leaves a record that `recover()` finishes, and two processes can never both
//! take the same step:
//!
//! ```text
//! QUOTED → DEST_INVOICE_CREATED → AUTHORIZED → SOURCE_PAYING → SOURCE_PAID → DEST_CLAIMED → COMPLETED
//!                                                            ↘ FAILED (nothing left the source)
//! a quote nobody confirmed in time → EXPIRED; paid but never claimed → NEEDS_ATTENTION
//! ```

use std::time::Duration;

use indigo_domain::{
    Error, IntentState, Msat, OpId, OpStatus, PayOp, PayQuote, RailId, ReceiveOp, Result, now,
};
use indigo_rail::MaybeSend;
use indigo_store::{IntentRecord, Store};

/// How long an unconfirmed move waits for the user, at most.
pub const CONFIRM_WINDOW_SECS: u64 = 60;

/// Test hook: `INDIGO_TEST_STOP_AT=SOURCE_PAID` ends the process the moment a
/// move reaches that state, like a crash (tests 2.6 and 2.9).
pub const STOP_AT_ENV: &str = "INDIGO_TEST_STOP_AT";
/// Test hook: `INDIGO_TEST_PAUSE_AT=AUTHORIZED:10` waits 10 seconds there, so
/// a test can stop an operator at that exact point (test 2.8).
pub const PAUSE_AT_ENV: &str = "INDIGO_TEST_PAUSE_AT";

/// The money calls a move needs. `indigo-core`'s wallet implements it; each
/// call records its operation as the wallet's own calls do.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Pockets: MaybeSend {
    async fn receive(&self, rail: &RailId, amount: Msat) -> Result<ReceiveOp>;
    async fn quote(&self, rail: &RailId, invoice: &str) -> Result<PayQuote>;
    /// Pays an authorized quote; the operation is recorded before paying.
    async fn pay(&self, quote: PayQuote) -> Result<PayOp>;
    /// Asks the operator where an operation stands; claims a paid receive.
    async fn refresh(&self, op: &OpId) -> Result<OpStatus>;
    /// Asks the operator again about an operation already recorded as
    /// expired or failed, and records it as completed if the operator now
    /// says so: for a move whose payment arrived after all.
    async fn recheck(&self, op: &OpId) -> Result<OpStatus>;
    /// Waits `d`, on whatever timer the platform has.
    async fn sleep(&self, d: Duration);
}

pub struct Engine<'a, P: Pockets> {
    store: &'a Store,
    pockets: &'a P,
}

impl<'a, P: Pockets> Engine<'a, P> {
    pub fn new(store: &'a Store, pockets: &'a P) -> Self {
        Self { store, pockets }
    }

    /// Starts a move: the destination issues an invoice and the source quotes
    /// paying it. Nothing is paid until `confirm`. With a `key` already used
    /// for the same move, returns that move as it stands instead of starting
    /// another; used for a different move, refuses.
    pub async fn start(&self, from: &RailId, to: &RailId, amount: Msat, key: Option<&str>) -> Result<IntentRecord> {
        if from == to {
            return Err(Error::InvalidInput("a move needs two different pockets".into()));
        }
        let id = format!("mv-{}", uuid::Uuid::now_v7().simple());
        let mut intent = self.store.create_intent(&IntentRecord {
            id: id.clone(),
            key: key.map(Into::into),
            from: from.clone(),
            to: to.clone(),
            amount,
            state: IntentState::Quoted,
            invoice: None,
            dest_op: None,
            quote_id: None,
            fee_reserve: None,
            quote_expires_at: None,
            source_op: None,
            note: None,
            created_at: now(),
            updated_at: now(),
        })?;
        if intent.id != id {
            if (&intent.from, &intent.to, intent.amount) != (from, to, amount) {
                return Err(Error::InvalidInput(format!(
                    "key {} was used for move {} ({} {} -> {}), not this one",
                    key.unwrap_or_default(),
                    intent.id,
                    intent.amount,
                    intent.from,
                    intent.to
                )));
            }
            return Ok(intent); // a used key: the existing move
        }
        stop_if(IntentState::Quoted);
        self.checkpoint(IntentState::Quoted).await;

        let receive = match self.pockets.receive(to, amount).await {
            Ok(receive) => receive,
            Err(e) => return self.fail(intent, IntentState::Quoted, format!("destination: {e}")),
        };
        let Some(invoice) = receive.invoice else {
            return self.fail(intent, IntentState::Quoted, "the destination gave no invoice".into());
        };
        intent.invoice = Some(invoice.clone());
        intent.dest_op = Some(receive.op);
        intent = self.advance_to(intent, IntentState::Quoted, IntentState::DestInvoiceCreated)?;

        match self.pockets.quote(from, &invoice).await {
            Ok(quote) => {
                intent.quote_id = Some(quote.quote_id);
                intent.fee_reserve = Some(quote.fee_reserve);
                intent.quote_expires_at = Some(quote.expires_at);
                self.save(&intent, IntentState::DestInvoiceCreated)?;
                self.checkpoint(IntentState::DestInvoiceCreated).await;
                Ok(intent)
            }
            Err(e) => self.fail(intent, IntentState::DestInvoiceCreated, format!("source: {e}")),
        }
    }

    /// Pays a started move and waits up to `wait` for the destination to
    /// claim it. Returns the move as it stands then.
    pub async fn confirm(&self, id: &str, wait: Duration) -> Result<IntentRecord> {
        let intent = self.load(id)?;
        if intent.state != IntentState::DestInvoiceCreated || intent.quote_id.is_none() {
            return Err(Error::InvalidInput(format!("move {id} is {} and can't be confirmed", intent.state)));
        }
        if self.unconfirmed_expired(&intent) {
            return self.finish(intent, IntentState::DestInvoiceCreated, IntentState::Expired, "not confirmed in time");
        }
        let mut intent = self.advance_to(intent, IntentState::DestInvoiceCreated, IntentState::Authorized)?;
        self.checkpoint(IntentState::Authorized).await;
        let quote = self.pay_quote(&intent)?;
        intent.source_op = Some(quote.op_id());
        // From here a payment may exist: recovery asks the source.
        let mut intent = self.advance_to(intent, IntentState::Authorized, IntentState::SourcePaying)?;
        self.checkpoint(IntentState::SourcePaying).await;
        match self.pay_while_destination_listens(&intent, quote).await {
            Ok(PayOp { status: OpStatus::Completed { .. }, .. }) => {
                intent = self.advance_to(intent, IntentState::SourcePaying, IntentState::SourcePaid)?;
            }
            Ok(PayOp { status: OpStatus::Pending, .. }) | Err(Error::OperatorUnavailable { .. }) => {}
            Ok(PayOp { status, .. }) => {
                let reason = format!("source payment {}", status.label().to_lowercase());
                return self.finish(intent, IntentState::SourcePaying, IntentState::Failed, &reason);
            }
            // Refused before any money moved.
            Err(e) => return self.finish(intent, IntentState::SourcePaying, IntentState::Failed, &e.to_string()),
        }
        self.settle(intent, wait).await
    }

    /// Takes a move as far as it can go without the user: what `recover`
    /// runs for every open move.
    pub async fn step(&self, intent: IntentRecord) -> Result<IntentRecord> {
        use IntentState::*;
        match intent.state {
            // Stopped before the destination's invoice was recorded: an
            // invoice it may have issued is never paid.
            Quoted => self.finish(intent, Quoted, Failed, "stopped before the destination issued an invoice"),
            DestInvoiceCreated if intent.quote_id.is_none() => {
                self.finish(intent, DestInvoiceCreated, Failed, "stopped before the source quoted")
            }
            DestInvoiceCreated if self.unconfirmed_expired(&intent) => {
                self.finish(intent, DestInvoiceCreated, Expired, "not confirmed in time")
            }
            // Waiting for the user, possibly in another process.
            DestInvoiceCreated => Ok(intent),
            // The payer saves SOURCE_PAYING before paying; winning this save
            // means it never will.
            Authorized => self.finish(intent, Authorized, Failed, "stopped before paying; nothing left the source"),
            SourcePaying => {
                let op = intent.source_op.clone().ok_or_else(|| Error::Other("paying move without a payment".into()))?;
                match self.pockets.refresh(&op).await {
                    Ok(OpStatus::Completed { .. }) => {
                        let intent = self.advance_to(intent, SourcePaying, SourcePaid)?;
                        Box::pin(self.step(intent)).await
                    }
                    Ok(OpStatus::Pending) => Ok(intent),
                    Ok(status) => {
                        let reason = format!("source payment {}", status.label().to_lowercase());
                        self.finish(intent, SourcePaying, Failed, &reason)
                    }
                    // Stopped between saving SOURCE_PAYING and recording the
                    // payment. Once the quote has expired it can't be paid.
                    Err(Error::UnknownOp(_)) if intent.quote_expires_at.is_some_and(|t| t < now()) => {
                        self.finish(intent, SourcePaying, Failed, "stopped before paying; nothing left the source")
                    }
                    Err(Error::UnknownOp(_)) | Err(Error::OperatorUnavailable { .. }) => Ok(intent),
                    Err(e) => Err(e),
                }
            }
            SourcePaid | NeedsAttention => {
                let from = intent.state;
                let op = intent.dest_op.clone().ok_or_else(|| Error::Other("move without a destination".into()))?;
                // Once the destination's record is final, only the operator
                // can still change the answer.
                let status = match from {
                    NeedsAttention => self.pockets.recheck(&op).await,
                    _ => self.pockets.refresh(&op).await,
                };
                match status {
                    Ok(OpStatus::Completed { .. }) => {
                        let intent = self.advance_to(intent, from, DestClaimed)?;
                        self.advance_to(intent, DestClaimed, Completed)
                    }
                    Ok(OpStatus::Pending) | Err(Error::OperatorUnavailable { .. }) => Ok(intent),
                    Ok(status) if from == SourcePaid => {
                        let reason = format!("paid, but the destination says {}", status.label().to_lowercase());
                        self.finish(intent, SourcePaid, NeedsAttention, &reason)
                    }
                    Ok(_) => Ok(intent),
                    Err(e) => Err(e),
                }
            }
            DestClaimed => self.advance_to(intent, DestClaimed, Completed),
            Completed | Failed | Expired => Ok(intent),
        }
    }

    /// Steps every open move. Errors are returned per move, not raised.
    pub async fn recover(&self) -> Result<Vec<(IntentRecord, Option<Error>)>> {
        let mut out = Vec::new();
        for intent in self.store.open_intents()? {
            let id = intent.id.clone();
            match self.step(intent).await {
                Ok(stepped) => out.push((stepped, None)),
                Err(e) => out.push((self.load(&id)?, Some(e))),
            }
        }
        Ok(out)
    }

    /// Steps a paid move until the destination claims it or `wait` passes.
    /// Counts its own one-second waits: no clock, so it runs in browsers too.
    async fn settle(&self, mut intent: IntentRecord, wait: Duration) -> Result<IntentRecord> {
        for _ in 0..wait.as_secs().max(1) {
            intent = self.step(intent).await?;
            if intent.state.is_final() {
                break;
            }
            self.pockets.sleep(Duration::from_secs(1)).await;
        }
        Ok(intent)
    }

    /// Pays, while asking the destination about its receive once a second.
    /// Some receivers must be online for a payment to land (a Bark server
    /// cancels an incoming payment its wallet is not there to take); for the
    /// others these questions just report it pending.
    ///
    /// The listening stops between questions, never during one: a question
    /// dropped half-way can leave the destination's own locks held.
    async fn pay_while_destination_listens(&self, intent: &IntentRecord, quote: PayQuote) -> Result<PayOp> {
        let paid = std::sync::atomic::AtomicBool::new(false);
        let pay = async {
            let result = self.pockets.pay(quote).await;
            paid.store(true, std::sync::atomic::Ordering::SeqCst);
            result
        };
        let listen = async {
            while !paid.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(op) = &intent.dest_op {
                    let _ = self.pockets.refresh(op).await;
                }
                self.pockets.sleep(Duration::from_secs(1)).await;
            }
        };
        let (result, ()) = futures_util::future::join(pay, listen).await;
        result
    }

    /// Runs the `INDIGO_TEST_PAUSE_AT` hook after a state was saved.
    async fn checkpoint(&self, state: IntentState) {
        let Ok(pause) = std::env::var(PAUSE_AT_ENV) else { return };
        if let Some((at, secs)) = pause.split_once(':')
            && at == state.as_str()
        {
            eprintln!("test hook: paused at {at} for {secs}s");
            self.pockets.sleep(Duration::from_secs(secs.parse().unwrap_or(10))).await;
        }
    }

    fn pay_quote(&self, intent: &IntentRecord) -> Result<PayQuote> {
        let missing = || Error::Other(format!("move {} has no quote", intent.id));
        Ok(PayQuote {
            rail: intent.from.clone(),
            quote_id: intent.quote_id.clone().ok_or_else(missing)?,
            invoice: indigo_domain::Bolt11::parse(intent.invoice.as_deref().ok_or_else(missing)?)?,
            amount: intent.amount,
            fee_reserve: intent.fee_reserve.ok_or_else(missing)?,
            expires_at: intent.quote_expires_at.ok_or_else(missing)?,
        })
    }

    fn unconfirmed_expired(&self, intent: &IntentRecord) -> bool {
        let quote_expires = intent.quote_expires_at.unwrap_or(0);
        now() >= quote_expires.min(intent.updated_at + CONFIRM_WINDOW_SECS)
    }

    fn load(&self, id: &str) -> Result<IntentRecord> {
        self.store.intent(id)?.ok_or_else(|| Error::InvalidInput(format!("no move {id}")))
    }

    fn save(&self, intent: &IntentRecord, from: IntentState) -> Result<()> {
        if self.store.save_intent(intent, from)? {
            Ok(())
        } else {
            let now_is = self.load(&intent.id)?.state;
            Err(Error::Other(format!("move {} changed elsewhere: it is {now_is}, not {from}", intent.id)))
        }
    }

    fn advance_to(&self, mut intent: IntentRecord, from: IntentState, to: IntentState) -> Result<IntentRecord> {
        intent.state = to;
        intent.updated_at = now();
        self.save(&intent, from)?;
        stop_if(to);
        Ok(intent)
    }

    fn finish(&self, mut intent: IntentRecord, from: IntentState, to: IntentState, note: &str) -> Result<IntentRecord> {
        intent.note = Some(note.to_string());
        self.advance_to(intent, from, to)
    }

    fn fail(&self, intent: IntentRecord, from: IntentState, note: String) -> Result<IntentRecord> {
        let failed = self.finish(intent, from, IntentState::Failed, &note)?;
        Err(Error::Other(format!("move {} failed: {note}", failed.id)))
    }
}


/// Runs the `INDIGO_TEST_STOP_AT` hook after a state was saved.
fn stop_if(state: IntentState) {
    if std::env::var(STOP_AT_ENV).is_ok_and(|stop| stop == state.as_str()) {
        // A crash, not a shutdown: nothing after this point runs.
        std::process::abort();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use super::*;

    const INVOICE: &str = "lnbcrt50u1p4dmad0pp5lsew5t0t58jhm4zs39qaxnlzn9960qj7mpa6kzqdmqgkvaqfy5ysdqqcqzzsxq97zvuqsp5h6utuyy47hn76xerzry2y5vjz343w30jmqhtunms4y8metwdzuhq9qxpqysgqy2r3zjhq0per6wu4t0l03p6fmqdf4ljf622aeunfgvj92lephqdqph6kx8q5h4mlxpuufv39u9sk8zr0rgtkdv5f0wl0sjrd8fl6hcqp82hw6v";

    /// Two pockets on paper. `paid` counts payments that really happened.
    #[derive(Default)]
    struct Mock {
        dest_down: bool,
        pay_refused: bool,
        dest_never_claims: bool,
        /// The destination's operator says "claimed" when asked again.
        claimed_late: Mutex<bool>,
        paid: Mutex<u32>,
        recorded: Mutex<Vec<OpId>>,
    }

    fn rail(name: &str) -> RailId {
        RailId(format!("{name}:test"))
    }

    #[async_trait::async_trait]
    impl Pockets for Mock {
        async fn receive(&self, _: &RailId, amount: Msat) -> Result<ReceiveOp> {
            if self.dest_down {
                return Err(Error::OperatorUnavailable { rail: rail("dest"), reason: "down".into() });
            }
            Ok(ReceiveOp { op: OpId("dest:test:receive:1".into()), amount, invoice: Some(INVOICE.into()), status: OpStatus::Pending })
        }

        async fn quote(&self, rail: &RailId, invoice: &str) -> Result<PayQuote> {
            Ok(PayQuote {
                rail: rail.clone(),
                quote_id: "q1".into(),
                invoice: indigo_domain::Bolt11::parse(invoice)?,
                amount: Msat::from_sats(5_000),
                fee_reserve: Msat::from_sats(10),
                expires_at: now() + 600,
            })
        }

        async fn pay(&self, quote: PayQuote) -> Result<PayOp> {
            self.recorded.lock().unwrap().push(quote.op_id());
            if self.pay_refused {
                return Err(Error::OperatorError { rail: quote.rail, reason: "no route".into() });
            }
            *self.paid.lock().unwrap() += 1;
            Ok(PayOp { op: quote.op_id(), status: OpStatus::Completed { amount: quote.amount, fee: Msat(1_000) } })
        }

        async fn refresh(&self, op: &OpId) -> Result<OpStatus> {
            let paid = *self.paid.lock().unwrap() > 0;
            if op.0.starts_with("dest") {
                return Ok(match (paid, self.dest_never_claims) {
                    (true, false) => OpStatus::Completed { amount: Msat::from_sats(5_000), fee: Msat::ZERO },
                    (true, true) => OpStatus::Expired,
                    (false, _) => OpStatus::Pending,
                });
            }
            if !self.recorded.lock().unwrap().contains(op) {
                return Err(Error::UnknownOp(op.clone()));
            }
            Ok(if paid { OpStatus::Completed { amount: Msat::from_sats(5_000), fee: Msat(1_000) } } else { OpStatus::Pending })
        }

        async fn recheck(&self, op: &OpId) -> Result<OpStatus> {
            if *self.claimed_late.lock().unwrap() {
                return Ok(OpStatus::Completed { amount: Msat::from_sats(5_000), fee: Msat::ZERO });
            }
            self.refresh(op).await
        }

        async fn sleep(&self, _: Duration) {}
    }

    fn store() -> Store {
        Store::open(Path::new(":memory:")).unwrap()
    }

    async fn start(engine: &Engine<'_, Mock>, key: Option<&str>) -> IntentRecord {
        engine.start(&rail("source"), &rail("dest"), Msat::from_sats(5_000), key).await.unwrap()
    }

    #[tokio::test]
    async fn a_confirmed_move_completes() {
        let (s, m) = (store(), Mock::default());
        let engine = Engine::new(&s, &m);
        let started = start(&engine, None).await;
        assert_eq!(started.state, IntentState::DestInvoiceCreated);
        assert_eq!(started.fee_reserve, Some(Msat::from_sats(10)));
        assert_eq!(*m.paid.lock().unwrap(), 0, "nothing is paid before confirm");
        let done = engine.confirm(&started.id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(done.state, IntentState::Completed);
        assert_eq!(*m.paid.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn a_used_key_never_pays_twice() {
        let (s, m) = (store(), Mock::default());
        let engine = Engine::new(&s, &m);
        let first = start(&engine, Some("k1")).await;
        engine.confirm(&first.id, Duration::from_secs(5)).await.unwrap();
        let again = start(&engine, Some("k1")).await;
        assert_eq!(again.id, first.id);
        assert_eq!(again.state, IntentState::Completed);
        assert!(engine.confirm(&again.id, Duration::from_secs(5)).await.is_err());
        assert_eq!(*m.paid.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn destination_down_fails_before_anything_is_paid() {
        let (s, m) = (store(), Mock { dest_down: true, ..Default::default() });
        let engine = Engine::new(&s, &m);
        assert!(engine.start(&rail("source"), &rail("dest"), Msat::from_sats(5_000), None).await.is_err());
        assert_eq!(s.intents().unwrap()[0].state, IntentState::Failed);
        assert_eq!(*m.paid.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_refused_payment_fails_the_move() {
        let (s, m) = (store(), Mock { pay_refused: true, ..Default::default() });
        let engine = Engine::new(&s, &m);
        let started = start(&engine, None).await;
        let done = engine.confirm(&started.id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(done.state, IntentState::Failed);
    }

    #[tokio::test]
    async fn paid_but_unclaimed_needs_attention() {
        let (s, m) = (store(), Mock { dest_never_claims: true, ..Default::default() });
        let engine = Engine::new(&s, &m);
        let started = start(&engine, None).await;
        let done = engine.confirm(&started.id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(done.state, IntentState::NeedsAttention);
        assert!(!done.state.is_final(), "checked again on every start");
        // The destination's operator later says it was claimed after all.
        *m.claimed_late.lock().unwrap() = true;
        let (recovered, _) = engine.recover().await.unwrap().remove(0);
        assert_eq!(recovered.state, IntentState::Completed);
    }

    #[tokio::test]
    async fn a_used_key_for_a_different_move_is_refused() {
        let (s, m) = (store(), Mock::default());
        let engine = Engine::new(&s, &m);
        start(&engine, Some("k2")).await;
        let other = engine.start(&rail("dest"), &rail("source"), Msat::from_sats(20_000), Some("k2")).await;
        assert!(matches!(other, Err(Error::InvalidInput(_))));
        assert_eq!(s.intents().unwrap().len(), 1);
    }

    /// A crash leaves the move in each state in turn; recovery must end it
    /// paid-and-claimed or with nothing paid, never in between.
    #[tokio::test]
    async fn recovery_finishes_a_move_stopped_in_any_state() {
        for stopped_in in [
            IntentState::Quoted,
            IntentState::Authorized,
            IntentState::SourcePaying,
            IntentState::SourcePaid,
            IntentState::DestClaimed,
        ] {
            let (s, m) = (store(), Mock::default());
            let engine = Engine::new(&s, &m);
            let mut intent = start(&engine, None).await;
            let read_in = intent.state;
            intent.state = stopped_in;
            intent.source_op = Some(engine.pay_quote(&intent).unwrap().op_id());
            if matches!(stopped_in, IntentState::SourcePaid | IntentState::DestClaimed) {
                m.pay(engine.pay_quote(&intent).unwrap()).await.unwrap();
            }
            assert!(s.save_intent(&intent, read_in).unwrap());

            let (recovered, error) = engine.recover().await.unwrap().remove(0);
            assert!(error.is_none(), "{stopped_in}: {error:?}");
            let paid = *m.paid.lock().unwrap();
            match recovered.state {
                IntentState::Completed => assert_eq!(paid, 1, "{stopped_in}"),
                IntentState::Failed => assert_eq!(paid, 0, "{stopped_in}"),
                // Stopped between saving SOURCE_PAYING and recording the payment:
                // it waits for the quote to expire.
                IntentState::SourcePaying => assert_eq!(paid, 0, "{stopped_in}"),
                other => panic!("stopped in {stopped_in}, recovered to {other}"),
            }
        }
    }
}
