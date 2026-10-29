//! Chooses which pocket pays or receives, and when no single pocket can pay,
//! which pockets top one up first. Pure decisions: the wallet (indigo-core)
//! asks the pockets for balances and quotes, then carries the plan out.

use indigo_domain::Msat;

/// What a pasted or scanned text is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A Lightning invoice (BOLT11).
    Lightning(String),
    /// An Ark address: paid Ark to Ark.
    Ark(String),
    /// A Bitcoin on-chain address.
    Bitcoin(String),
    /// A Cashu token to redeem.
    CashuToken(String),
    /// Fedimint notes to redeem.
    FedimintNotes(String),
}

impl Destination {
    /// Recognises a text by its prefix; `None` if it is none of the above.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let lower = text.to_ascii_lowercase();
        let strip = |scheme: &str| lower.strip_prefix(scheme).map(|_| text[scheme.len()..].to_string());
        if let Some(rest) = strip("lightning:") {
            return Self::parse(&rest);
        }
        if let Some(rest) = strip("bitcoin:") {
            let address = rest.split('?').next().unwrap_or_default();
            return Some(Destination::Bitcoin(address.to_string()));
        }
        if lower.starts_with("cashua") || lower.starts_with("cashub") {
            Some(Destination::CashuToken(text.to_string()))
        } else if lower.starts_with("fedimint") {
            Some(Destination::FedimintNotes(text.to_string()))
        } else if lower.starts_with("ark1") || lower.starts_with("tark1") {
            Some(Destination::Ark(text.to_string()))
        } else if lower.starts_with("ln") && !lower.starts_with("lnurl") {
            Some(Destination::Lightning(text.to_string()))
        } else if ["bc1", "tb1", "bcrt1"].iter().any(|p| lower.starts_with(p))
            || ((26..=35).contains(&text.len()) && text.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            Some(Destination::Bitcoin(text.to_string()))
        } else {
            None
        }
    }
}

/// A pocket and what it can spend now.
#[derive(Clone, Debug)]
pub struct Pocket {
    pub name: String,
    pub spendable: Msat,
}

/// A pocket that can pay the destination, and what paying from it costs in
/// total (amount plus its fee).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub cost: Msat,
}

/// Moves `amount` from one pocket into the paying one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopUp {
    pub from: String,
    pub amount: Msat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// One pocket covers it.
    Pay { from: String },
    /// No pocket covers it alone: move money into `into` first, then pay.
    TopUpThenPay { into: String, moves: Vec<TopUp> },
    /// All pockets together cannot cover it.
    CannotCover { have: Msat, need: Msat },
}

/// What a move between pockets may cost the sending pocket: Lightning fees,
/// estimated high.
fn move_cost(amount: u64) -> u64 {
    amount + amount / 50 + 50_000
}

/// The receiving pocket can charge too (Bark takes about 100 sats to receive
/// over Lightning), so a top-up brings in more than the shortfall...
fn top_up_margin(shortfall: u64) -> u64 {
    shortfall / 50 + 200_000
}

/// ...and is never so small that the receiving fee eats it.
const MIN_TOP_UP: u64 = 1_000_000;

/// Round up to whole sats: Cashu and Bark only move whole sats.
fn whole_sats(msat: u64) -> u64 {
    msat.div_ceil(1000) * 1000
}

/// Picks how to pay. The cheapest candidate that covers its own cost pays;
/// otherwise the best-funded candidate is topped up from the other pockets,
/// richest first.
pub fn plan(pockets: &[Pocket], candidates: &[Candidate]) -> Plan {
    let spendable = |name: &str| pockets.iter().find(|p| p.name == name).map_or(0, |p| p.spendable.0);
    let covering = candidates.iter().filter(|c| spendable(&c.name) >= c.cost.0).min_by_key(|c| c.cost.0);
    if let Some(c) = covering {
        return Plan::Pay { from: c.name.clone() };
    }
    let Some(into) = candidates.iter().max_by_key(|c| (spendable(&c.name), std::cmp::Reverse(c.cost.0))) else {
        return Plan::CannotCover { have: Msat(0), need: Msat(0) };
    };
    let shortfall = into.cost.0 - spendable(&into.name);
    let mut needed = whole_sats(shortfall + top_up_margin(shortfall)).max(MIN_TOP_UP);
    let mut others: Vec<&Pocket> = pockets.iter().filter(|p| p.name != into.name).collect();
    others.sort_by_key(|p| std::cmp::Reverse(p.spendable.0));
    let mut moves = Vec::new();
    for p in others {
        if needed == 0 {
            break;
        }
        // The most this pocket can send once its own fee is paid.
        let mut can_give = p.spendable.0.saturating_sub(50_000) * 50 / 51 / 1000 * 1000;
        while can_give > 0 && move_cost(can_give) > p.spendable.0 {
            can_give -= 1000;
        }
        let amount = needed.min(can_give);
        if amount > 0 {
            moves.push(TopUp { from: p.name.clone(), amount: Msat(amount) });
            needed -= amount;
        }
    }
    if needed > 0 {
        let have = pockets.iter().map(|p| p.spendable.0).sum();
        return Plan::CannotCover { have: Msat(have), need: into.cost };
    }
    Plan::TopUpThenPay { into: into.name.clone(), moves }
}

/// At or above this, a Lightning receive goes to Ark (self-custody) when the
/// wallet has an Ark pocket; smaller amounts go to ecash, which is instant
/// and free to receive.
pub const LARGE_RECEIVE: Msat = Msat(100_000_000);

/// Which pocket receives `amount` over Lightning, from the wallet's pockets
/// (named `cashu`, `fedimint`, `ark-...`, optionally followed by `:<where>`).
pub fn receive_pocket(amount: Msat, pockets: &[String]) -> Option<String> {
    let has = |name: &str| pockets.iter().find(|p| p.split(':').next() == Some(name)).cloned();
    let ark = pockets.iter().find(|p| p.starts_with("ark")).cloned();
    if amount.0 >= LARGE_RECEIVE.0 {
        if let Some(ark) = ark {
            return Some(ark);
        }
    }
    has("cashu").or_else(|| has("fedimint")).or(ark).or_else(|| pockets.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sats(n: u64) -> Msat {
        Msat::from_sats(n)
    }

    fn pockets(list: &[(&str, u64)]) -> Vec<Pocket> {
        list.iter().map(|(n, s)| Pocket { name: n.to_string(), spendable: sats(*s) }).collect()
    }

    fn cand(name: &str, cost: u64) -> Candidate {
        Candidate { name: name.into(), cost: sats(cost) }
    }

    #[test]
    fn recognises_what_was_pasted() {
        assert!(matches!(Destination::parse(" lnbcrt500u1p4wr "), Some(Destination::Lightning(_))));
        assert!(matches!(Destination::parse("LIGHTNING:lnbc1abc"), Some(Destination::Lightning(s)) if s == "lnbc1abc"));
        assert!(matches!(Destination::parse("tark1qqabc"), Some(Destination::Ark(_))));
        assert!(matches!(Destination::parse("cashuBo2Ftdmh0"), Some(Destination::CashuToken(_))));
        assert!(matches!(Destination::parse("fedimint70002edr"), Some(Destination::FedimintNotes(_))));
        assert!(matches!(Destination::parse("bitcoin:bcrt1qxyz?amount=1"), Some(Destination::Bitcoin(a)) if a == "bcrt1qxyz"));
        assert_eq!(Destination::parse("lnurl1dp68gurn"), None);
        assert_eq!(Destination::parse("hello there"), None);
    }

    #[test]
    fn the_cheapest_covering_pocket_pays() {
        let p = pockets(&[("cashu", 10_000), ("fedimint", 10_000), ("ark-bark", 500)]);
        let plan = plan(&p, &[cand("cashu", 5_050), cand("fedimint", 5_010), cand("ark-bark", 5_090)]);
        assert_eq!(plan, Plan::Pay { from: "fedimint".into() });
    }

    #[test]
    fn tops_up_the_best_funded_pocket_when_none_covers() {
        let p = pockets(&[("cashu", 6_000), ("fedimint", 5_000), ("ark-bark", 1_000)]);
        let Plan::TopUpThenPay { into, moves } = plan(&p, &[cand("cashu", 9_010), cand("fedimint", 9_005)]) else {
            panic!("expected a top-up");
        };
        assert_eq!(into, "cashu");
        // Shortfall 3,010 plus margin, from fedimint (the richest other pocket).
        assert_eq!(moves, vec![TopUp { from: "fedimint".into(), amount: sats(3_271) }]);
    }

    #[test]
    fn an_ark_address_tops_up_ark_from_several_pockets() {
        let p = pockets(&[("cashu", 3_000), ("fedimint", 2_000), ("ark-bark", 1_000)]);
        let Plan::TopUpThenPay { into, moves } = plan(&p, &[cand("ark-bark", 4_000)]) else {
            panic!("expected a top-up");
        };
        assert_eq!(into, "ark-bark");
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].from, "cashu");
        let total: u64 = moves.iter().map(|m| m.amount.0).sum();
        assert!(total >= sats(3_000).0);
    }

    #[test]
    fn a_top_up_is_never_tiny() {
        let p = pockets(&[("cashu", 9_000), ("ark-bark", 1_990)]);
        let Plan::TopUpThenPay { moves, .. } = plan(&p, &[cand("ark-bark", 2_000)]) else {
            panic!("expected a top-up");
        };
        assert_eq!(moves[0].amount, sats(1_000));
    }

    #[test]
    fn says_so_when_everything_together_is_not_enough() {
        let p = pockets(&[("cashu", 1_000), ("fedimint", 1_000)]);
        assert!(matches!(plan(&p, &[cand("cashu", 5_000)]), Plan::CannotCover { .. }));
    }

    #[test]
    fn small_receives_go_to_ecash_large_ones_to_ark() {
        let all = vec!["cashu".to_string(), "fedimint".to_string(), "ark-bark".to_string()];
        assert_eq!(receive_pocket(sats(5_000), &all).as_deref(), Some("cashu"));
        assert_eq!(receive_pocket(sats(250_000), &all).as_deref(), Some("ark-bark"));
        assert_eq!(receive_pocket(sats(5_000), &all[2..]).as_deref(), Some("ark-bark"));
    }
}
