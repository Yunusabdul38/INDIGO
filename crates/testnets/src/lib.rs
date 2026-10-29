//! Endpoints of the public test networks and the local regtest stack, plus
//! test-only descriptors. One place for every test, spike and app, so a
//! rotated invite or a moved mint is changed once.

/// Mutinynet (a signet with 30-second blocks) and its public test services.
pub mod mutinynet {
    pub const ESPLORA: &str = "https://mutinynet.com/api";
    /// The public test mint. As of 2026-10-29 it cannot create Lightning
    /// invoices (its own Lightning backend's certificate expired) and it sends
    /// a duplicate CORS header that browsers reject.
    pub const CASHU_MINT: &str = "https://cashu.mutinynet.com";
    /// Our own mint with a real Lightning node: infra/docker-compose.mutinynet.yml.
    pub const OWN_CASHU_MINT: &str = "http://127.0.0.1:28085";
    pub const ARKADE: &str = "https://mutinynet.arkade.sh";
    /// Boltz for Arkade on Mutinynet: Arkade's Lightning goes through swaps.
    pub const ARKADE_BOLTZ: &str = "https://api.boltz.mutinynet.arkade.sh";
    pub const FAUCET_WEBSITE: &str = "https://faucet.mutinynet.com";
    /// The faucet federation, as published on the faucet website.
    pub const FAUCET_FEDERATION_INVITE: &str = "fed11qgqzc2nhwden5te0vejkg6tdd9h8gepwvejkg6tdd9h8garhduhx6at5d9h8jmn9wshxxmmd9uqqzgxg6s3evnr6m9zdxr6hxkdkukexpcs3mn7mj3g5pc5dfh63l4tj6g9zk4er";
    /// Guardian API of that federation (the first peer in its invite).
    pub const FAUCET_FEDERATION_GUARDIAN: &str = "wss://fedimintd.fediminttwo.mutinynet.com/";
    /// Our own federation and its Lightning gateway: infra/docker-compose.mutinynet.yml.
    /// Its invite code is written to infra/.mutinynet-federation-invite by setup.
    pub const OWN_FEDIMINT_GUARDIAN: &str = "ws://127.0.0.1:28174/";
    pub const OWN_FEDIMINT_GATEWAY: &str = "http://127.0.0.1:28176";
}

/// Second's public signet (Bitcoin's default signet) and its Ark server.
/// A different chain from Mutinynet. Its faucet needs a GitHub login.
pub mod signet {
    pub const ESPLORA: &str = "https://esplora.signet.2nd.dev";
    pub const BARK_SERVER: &str = "https://ark.signet.2nd.dev";
    pub const FAUCET_WEBSITE: &str = "https://signet.2nd.dev";
}

/// The local stack in infra/docker-compose.regtest.yml. Ports match that file.
pub mod regtest {
    pub const BITCOIND_RPC: &str = "http://127.0.0.1:18443";
    pub const BITCOIND_USER: &str = "indigo";
    pub const BITCOIND_PASS: &str = "indigo";
    pub const ESPLORA: &str = "http://127.0.0.1:50002";
    pub const CASHU_MINT: &str = "http://127.0.0.1:18085";
    pub const FEDIMINT_GUARDIAN: &str = "ws://127.0.0.1:18174/";
    pub const FEDIMINT_GATEWAY: &str = "http://127.0.0.1:18176";
    pub const BARK_SERVER: &str = "http://127.0.0.1:18535";
    /// The Bark server's bitcoind (v31, `-txindex`): Bark wallets read the
    /// chain here, since our Esplora lacks the package relay Bark needs.
    pub const BARK_BITCOIND_RPC: &str = "http://127.0.0.1:18453";
    pub const LND_REST: &str = "https://127.0.0.1:18080";
}

/// BIP84 test descriptors from a well-known test vector. Never hold real funds.
pub mod test_descriptors {
    pub const BIP84_RECEIVE: &str = "wpkh(tprv8ZgxMBicQKsPdy6LMhUtFHAgpocR8GC6QmwMSFpZs7h6Eziw3SpThFfczTDh5rW2krkqffa11UpX3XkeTTB2FvzZKWXqPY54Y6Rq4AQ5R8L/84'/1'/0'/0/*)";
    pub const BIP84_CHANGE: &str = "wpkh(tprv8ZgxMBicQKsPdy6LMhUtFHAgpocR8GC6QmwMSFpZs7h6Eziw3SpThFfczTDh5rW2krkqffa11UpX3XkeTTB2FvzZKWXqPY54Y6Rq4AQ5R8L/84'/1'/0'/1/*)";
}
