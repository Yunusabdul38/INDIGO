//! Service endpoints per test network. The addresses themselves live in
//! `indigo-testnets`, shared with the tests and apps.

use clap::ValueEnum;
use indigo_testnets::{mutinynet, regtest, signet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Network {
    Mutinynet,
    /// Second's public signet: only its Ark server (Bark) runs there.
    Signet,
    Regtest,
}

/// Where a Bark wallet reads the chain (test credentials are fixed).
#[derive(Clone, Copy)]
pub enum BarkChainSource {
    Esplora(&'static str),
    Bitcoind(&'static str),
}

/// Where the chain tip comes from.
pub enum Chain {
    Esplora(&'static str),
    BitcoindRpc {
        url: &'static str,
        user: &'static str,
        pass: &'static str,
    },
}

pub struct Endpoints {
    pub chain: Chain,
    /// Esplora index in front of a bitcoind chain, regtest only (on Mutinynet
    /// the chain check already reads Esplora). BDK, Bark, Arkade and Fedimint
    /// v1 wallets read the chain through it.
    pub esplora_index: Option<&'static str>,
    pub cashu_mint: Option<&'static str>,
    /// The Bark (Ark) server and where its wallets read the chain.
    pub bark: Option<(&'static str, BarkChainSource)>,
    /// The Arkade operator, its Boltz, and Esplora.
    pub arkade_pocket: Option<(&'static str, &'static str, &'static str)>,
    /// Guardian API of the default federation (the first peer in its invite).
    pub fedimint_guardian: Option<&'static str>,
    /// That federation's Lightning gateway.
    pub fedimint_gateway: Option<&'static str>,
    pub arkade: Option<&'static str>,
    /// LND REST, regtest only.
    pub lightning: Option<&'static str>,
    /// Faucet website (on-chain and Lightning coins), Mutinynet only.
    pub faucet: Option<&'static str>,
}

impl Network {
    /// The Bitcoin network a wallet on this test network is bound to.
    pub fn bitcoin(self) -> indigo_domain::Network {
        match self {
            // Mutinynet is a signet.
            Network::Mutinynet | Network::Signet => indigo_domain::Network::Signet,
            Network::Regtest => indigo_domain::Network::Regtest,
        }
    }

    pub fn endpoints(self) -> Endpoints {
        match self {
            Network::Mutinynet => Endpoints {
                chain: Chain::Esplora(mutinynet::ESPLORA),
                esplora_index: None,
                cashu_mint: Some(mutinynet::OWN_CASHU_MINT),
                bark: None,
                arkade_pocket: Some((mutinynet::ARKADE, mutinynet::ARKADE_BOLTZ, mutinynet::ESPLORA)),
                fedimint_guardian: Some(mutinynet::OWN_FEDIMINT_GUARDIAN),
                fedimint_gateway: Some(mutinynet::OWN_FEDIMINT_GATEWAY),
                arkade: Some(mutinynet::ARKADE),
                lightning: None,
                faucet: Some(mutinynet::FAUCET_WEBSITE),
            },
            Network::Signet => Endpoints {
                chain: Chain::Esplora(signet::ESPLORA),
                esplora_index: None,
                cashu_mint: None,
                bark: Some((signet::BARK_SERVER, BarkChainSource::Esplora(signet::ESPLORA))),
                arkade_pocket: None,
                fedimint_guardian: None,
                fedimint_gateway: None,
                arkade: None,
                lightning: None,
                faucet: Some(signet::FAUCET_WEBSITE),
            },
            Network::Regtest => Endpoints {
                chain: Chain::BitcoindRpc {
                    url: regtest::BITCOIND_RPC,
                    user: regtest::BITCOIND_USER,
                    pass: regtest::BITCOIND_PASS,
                },
                esplora_index: Some(regtest::ESPLORA),
                cashu_mint: Some(regtest::CASHU_MINT),
                bark: Some((regtest::BARK_SERVER, BarkChainSource::Bitcoind(regtest::BARK_BITCOIND_RPC))),
                arkade_pocket: None,
                fedimint_guardian: Some(regtest::FEDIMINT_GUARDIAN),
                fedimint_gateway: Some(regtest::FEDIMINT_GATEWAY),
                arkade: None,
                lightning: Some(regtest::LND_REST),
                faucet: None,
            },
        }
    }
}
