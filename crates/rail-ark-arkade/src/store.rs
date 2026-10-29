//! Arkade's swap and contract records, persisted in this pocket's own SQLite
//! file. Arkade's own SQLite storage needs a newer rusqlite than the rest of
//! INDIGO links, so the records are kept here instead, as JSON.

use std::path::Path;
use std::sync::Mutex;

use ark_client::Error;
use ark_client::{ChainSwapData, ReverseSwapData, SubmarineSwapData, SwapStatus};
use ark_client::swap_storage::SwapStorage;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS swaps (kind TEXT NOT NULL, id TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (kind, id));
-- Transactions this pocket made for an INDIGO operation, so `recover` does
-- not report them again as arrivals.
CREATE TABLE IF NOT EXISTS links (txid TEXT PRIMARY KEY, op TEXT NOT NULL);
";

fn db(e: impl std::fmt::Display) -> Error {
    Error::consumer(format!("arkade store: {e}"))
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(path).map_err(db)?;
        conn.execute_batch(SCHEMA).map_err(db)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn link(&self, txid: &str, op: &str) -> Result<(), Error> {
        self.conn()
            .execute("INSERT OR IGNORE INTO links (txid, op) VALUES (?1, ?2)", params![txid, op])
            .map_err(db)?;
        Ok(())
    }

    pub fn linked(&self, txid: &str) -> Result<Option<String>, Error> {
        self.conn().query_row("SELECT op FROM links WHERE txid = ?1", [txid], |r| r.get(0)).optional().map_err(db)
    }

    pub fn all_submarine(&self) -> Result<Vec<SubmarineSwapData>, Error> {
        self.all("submarine")
    }

    pub fn get_reverse_sync(&self, id: &str) -> Result<Option<ReverseSwapData>, Error> {
        self.get("reverse", id)
    }

    fn put<T: Serialize>(&self, kind: &str, id: &str, data: &T) -> Result<(), Error> {
        let json = serde_json::to_string(data).map_err(db)?;
        self.conn()
            .execute(
                "INSERT INTO swaps (kind, id, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (kind, id) DO UPDATE SET data = excluded.data",
                params![kind, id, json],
            )
            .map_err(db)?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>, Error> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT data FROM swaps WHERE kind = ?1 AND id = ?2", params![kind, id], |r| r.get(0))
            .optional()
            .map_err(db)?;
        json.map(|j| serde_json::from_str(&j).map_err(db)).transpose()
    }

    fn all<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, Error> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT data FROM swaps WHERE kind = ?1").map_err(db)?;
        let rows = stmt.query_map([kind], |r| r.get::<_, String>(0)).map_err(db)?;
        rows.map(|j| serde_json::from_str(&j.map_err(db)?).map_err(db)).collect()
    }

    fn remove<T: DeserializeOwned>(&self, kind: &str, id: &str) -> Result<Option<T>, Error> {
        let found = self.get(kind, id)?;
        self.conn().execute("DELETE FROM swaps WHERE kind = ?1 AND id = ?2", params![kind, id]).map_err(db)?;
        Ok(found)
    }
}

/// Each swap kind's status lives inside its record.
macro_rules! update_status {
    ($self:ident, $kind:literal, $ty:ty, $id:ident, $status:ident) => {{
        let mut data: $ty =
            $self.get($kind, $id)?.ok_or_else(|| Error::consumer(format!("no {} swap {}", $kind, $id)))?;
        data.status = $status;
        $self.put($kind, $id, &data)
    }};
}

#[async_trait::async_trait]
impl SwapStorage for Store {
    async fn insert_submarine(&self, id: String, data: SubmarineSwapData) -> Result<(), Error> {
        self.put("submarine", &id, &data)
    }
    async fn insert_reverse(&self, id: String, data: ReverseSwapData) -> Result<(), Error> {
        self.put("reverse", &id, &data)
    }
    async fn get_submarine(&self, id: &str) -> Result<Option<SubmarineSwapData>, Error> {
        self.get("submarine", id)
    }
    async fn get_reverse(&self, id: &str) -> Result<Option<ReverseSwapData>, Error> {
        self.get("reverse", id)
    }
    async fn update_status_submarine(&self, id: &str, status: SwapStatus) -> Result<(), Error> {
        update_status!(self, "submarine", SubmarineSwapData, id, status)
    }
    async fn update_status_reverse(&self, id: &str, status: SwapStatus) -> Result<(), Error> {
        update_status!(self, "reverse", ReverseSwapData, id, status)
    }
    async fn update_submarine(&self, id: &str, data: SubmarineSwapData) -> Result<(), Error> {
        self.put("submarine", id, &data)
    }
    async fn update_reverse(&self, id: &str, data: ReverseSwapData) -> Result<(), Error> {
        self.put("reverse", id, &data)
    }
    async fn list_all_submarine(&self) -> Result<Vec<SubmarineSwapData>, Error> {
        self.all("submarine")
    }
    async fn list_all_reverse(&self) -> Result<Vec<ReverseSwapData>, Error> {
        self.all("reverse")
    }
    async fn remove_submarine(&self, id: &str) -> Result<Option<SubmarineSwapData>, Error> {
        self.remove("submarine", id)
    }
    async fn remove_reverse(&self, id: &str) -> Result<Option<ReverseSwapData>, Error> {
        self.remove("reverse", id)
    }
    async fn insert_chain(&self, id: String, data: ChainSwapData) -> Result<(), Error> {
        self.put("chain", &id, &data)
    }
    async fn get_chain(&self, id: &str) -> Result<Option<ChainSwapData>, Error> {
        self.get("chain", id)
    }
    async fn update_status_chain(&self, id: &str, status: SwapStatus) -> Result<(), Error> {
        update_status!(self, "chain", ChainSwapData, id, status)
    }
    async fn update_chain(&self, id: &str, data: ChainSwapData) -> Result<(), Error> {
        self.put("chain", id, &data)
    }
    async fn list_all_chain(&self) -> Result<Vec<ChainSwapData>, Error> {
        self.all("chain")
    }
    async fn remove_chain(&self, id: &str) -> Result<Option<ChainSwapData>, Error> {
        self.remove("chain", id)
    }
}
