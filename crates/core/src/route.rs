//! Paying anything and receiving without naming a pocket. indigo-router
//! decides which pocket pays and what tops it up; this carries the plan out
//! with the same recorded operations and moves as everything else.

use std::time::Duration;

use indigo_domain::{Bolt11, Error, IntentState, Msat, NativeToken, OpStatus, PayQuote, RailId, ReceiveOp, Result, now};
use indigo_router::{Candidate, Destination, Plan, Pocket};

use crate::{IntentRecord, Wallet};

/// What a send did.
#[derive(Debug)]
pub struct SendReport {
    /// The pocket that paid.
    pub from: RailId,
    /// Moves that topped that pocket up first; each one COMPLETED.
    pub top_ups: Vec<IntentRecord>,
    pub status: OpStatus,
    /// For ecash: the token to hand over.
    pub token: Option<NativeToken>,
}

/// What is being paid.
enum Target {
    Invoice(Bolt11),
    Ark { address: String, amount: Msat },
    Ecash(Msat),
}

/// How long a top-up move may take before the send gives up on it.
const TOP_UP_WAIT: Duration = Duration::from_secs(90);

/// Rounds of top-up then re-check: a receiving pocket's fees can leave a
/// first top-up a little short.
const ROUNDS: usize = 3;

fn short(id: &RailId) -> &str {
    id.0.split(':').next().unwrap_or(&id.0)
}

impl Wallet {
    /// Pays a pasted text: a Lightning invoice or an Ark address. With
    /// `from`, only that pocket pays; without, the wallet picks the cheapest
    /// pocket that covers it, or tops one up from the others first.
    pub async fn send(&self, text: &str, amount: Option<Msat>, from: Option<&str>) -> Result<SendReport> {
        let target = match Destination::parse(text) {
            Some(Destination::Lightning(invoice)) => Target::Invoice(Bolt11::parse(&invoice)?),
            Some(Destination::Ark(address)) => {
                let amount = amount.ok_or_else(|| Error::InvalidInput("how much to send to this Ark address?".into()))?;
                Target::Ark { address, amount }
            }
            Some(Destination::Bitcoin(_)) => {
                return Err(Error::InvalidInput("on-chain sends are not routed automatically yet".into()));
            }
            Some(Destination::CashuToken(_) | Destination::FedimintNotes(_)) => {
                return Err(Error::InvalidInput("that is ecash to redeem, not something to pay".into()));
            }
            None => return Err(Error::InvalidInput("not a Lightning invoice or an Ark address".into())),
        };
        self.route(target, from).await
    }

    /// Creates ecash to hand over: from `from` (cashu or fedimint) or, without
    /// it, whichever ecash pocket covers it, topped up if need be.
    pub async fn send_ecash(&self, amount: Msat, from: Option<&str>) -> Result<SendReport> {
        self.route(Target::Ecash(amount), from).await
    }

    /// Redeems a pasted Cashu token or Fedimint notes into the matching pocket.
    pub async fn redeem(&self, text: &str) -> Result<ReceiveOp> {
        match Destination::parse(text) {
            Some(Destination::CashuToken(token)) => self.receive_token("cashu", &token).await,
            Some(Destination::FedimintNotes(notes)) => self.receive_token("fedimint", &notes).await,
            _ => Err(Error::InvalidInput("not a Cashu token or Fedimint notes".into())),
        }
    }

    /// A Lightning invoice into `into`, or into the pocket indigo-router picks.
    pub async fn receive_auto(&self, amount: Msat, into: Option<&str>, memo: &str) -> Result<ReceiveOp> {
        let names: Vec<String> = self.rails().iter().map(|r| r.id().0).collect();
        let into = match into {
            Some(into) => into.to_string(),
            None => indigo_router::receive_pocket(amount, &names)
                .ok_or_else(|| Error::InvalidInput("this wallet has no pocket to receive into".into()))?,
        };
        self.receive(&into, amount, memo).await
    }

    async fn route(&self, target: Target, only: Option<&str>) -> Result<SendReport> {
        let only = only.map(|name| self.rail(name).map(|r| r.id())).transpose()?;
        let mut top_ups = Vec::new();
        for _ in 0..ROUNDS {
            let pockets = self.pockets().await;
            let candidates = self.candidates(&target, only.as_ref()).await;
            if candidates.is_empty() {
                return Err(self.why_none(&target, only.as_ref(), &pockets));
            }
            let costs: Vec<Candidate> = candidates.iter().map(|(c, _)| c.clone()).collect();
            let plan = match &only {
                // A pocket the user picked pays alone, or not at all.
                Some(rail) => {
                    let have = pockets.iter().find(|p| p.name == rail.0).map_or(Msat(0), |p| p.spendable);
                    if have < costs[0].cost {
                        return Err(Error::InsufficientFunds { available: have, needed: costs[0].cost });
                    }
                    Plan::Pay { from: rail.0.clone() }
                }
                None => indigo_router::plan(&pockets, &costs),
            };
            match plan {
                Plan::Pay { from } => {
                    let quote = candidates.into_iter().find(|(c, _)| c.name == from).and_then(|(_, q)| q);
                    let (status, token) = self.pay_target(&target, &from, quote).await?;
                    return Ok(SendReport { from: RailId(from), top_ups, status, token });
                }
                Plan::TopUpThenPay { into, moves } => {
                    for m in moves {
                        let started = self.start_move(&m.from, &into, m.amount, None).await?;
                        let done = self.confirm_move(&started.id, TOP_UP_WAIT).await?;
                        if done.state != IntentState::Completed {
                            return Err(Error::Other(format!(
                                "topping up {} from {} ended {}: {}",
                                short(&done.to),
                                short(&done.from),
                                done.state.as_str(),
                                done.note.clone().unwrap_or_default()
                            )));
                        }
                        top_ups.push(done);
                    }
                }
                Plan::CannotCover { have, need } => {
                    return Err(Error::InsufficientFunds { available: have, needed: need });
                }
            }
        }
        Err(Error::Other("still short after topping up; try again".into()))
    }

    /// Why no pocket offered to pay: short of funds (a pocket refuses to
    /// quote what it can't cover), or this kind of pocket can't pay it.
    fn why_none(&self, target: &Target, only: Option<&RailId>, pockets: &[Pocket]) -> Error {
        let amount = match target {
            Target::Invoice(invoice) => invoice.amount().unwrap_or(Msat(0)),
            Target::Ark { amount, .. } | Target::Ecash(amount) => *amount,
        };
        let have = pockets.iter().filter(|p| only.is_none_or(|o| o.0 == p.name)).map(|p| p.spendable).max();
        match (have, only) {
            (Some(have), _) if have < amount => Error::InsufficientFunds { available: have, needed: amount },
            (_, Some(rail)) => Error::InvalidInput(format!("the {} pocket can't pay this", short(rail))),
            (_, None) => Error::InvalidInput("no pocket can pay this".into()),
        }
    }

    async fn pockets(&self) -> Vec<Pocket> {
        let mut out = Vec::new();
        for rail in self.rails() {
            if let Ok(b) = rail.balance().await {
                out.push(Pocket { name: rail.id().0, spendable: b.spendable });
            }
        }
        out
    }

    /// The pockets that can pay `target`, with what it costs from each (and
    /// the quote, for an invoice).
    async fn candidates(&self, target: &Target, only: Option<&RailId>) -> Vec<(Candidate, Option<PayQuote>)> {
        let mut out = Vec::new();
        for rail in self.rails() {
            let id = rail.id();
            if only.is_some_and(|o| *o != id) {
                continue;
            }
            let name = id.0.clone();
            match target {
                Target::Invoice(invoice) => {
                    if let Ok(q) = rail.ln_quote(invoice).await {
                        if let Ok(cost) = q.amount.checked_add(q.fee_reserve) {
                            out.push((Candidate { name, cost }, Some(q)));
                        }
                    }
                }
                Target::Ark { amount, .. } if short(&id).starts_with("ark") => {
                    out.push((Candidate { name, cost: *amount }, None));
                }
                // Fedimint notes come in fixed denominations: allow a little over.
                Target::Ecash(amount) if matches!(short(&id), "cashu" | "fedimint") => {
                    let cost = Msat(amount.0 + amount.0 / 100 + 10_000);
                    out.push((Candidate { name, cost }, None));
                }
                _ => {}
            }
        }
        out
    }

    async fn pay_target(
        &self,
        target: &Target,
        from: &str,
        quote: Option<PayQuote>,
    ) -> Result<(OpStatus, Option<NativeToken>)> {
        match target {
            Target::Invoice(invoice) => {
                // A quote from before a top-up may be stale; ask again.
                let quote = match quote {
                    Some(q) if q.expires_at > now() + 10 => q,
                    _ => self.rail(from)?.ln_quote(invoice).await?,
                };
                Ok((self.pay(quote, None).await?.status, None))
            }
            Target::Ark { address, amount } => Ok((self.pay_address(from, address, *amount).await?.status, None)),
            Target::Ecash(amount) => {
                let token = self.send_token(from, *amount).await?;
                Ok((OpStatus::Completed { amount: token.amount, fee: token.fee }, Some(token)))
            }
        }
    }
}
