//! The key/value settings table.
//!
//! Values are strings because the frontend reads and writes them as strings;
//! anything that needs a type parses at the point of use.

use super::*;

impl Database {
    // --- Config Operations (Settings) ---

    /// Get a config value by key
    pub fn get_config(&self, key: &str) -> Result<Option<String>> {
        let conn = self.get_conn()?;
        let result: rusqlite::Result<String> =
            conn.query_row("SELECT value FROM config WHERE key = ?1", [key], |row| {
                row.get(0)
            });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Set a config value
    pub fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value, updated_at) VALUES (?1, ?2, ?3)",
            (key, value, now),
        )?;
        Ok(())
    }

    /// Delete a config key
    pub fn remove_config(&self, key: &str) -> Result<()> {
        let conn = self.get_conn()?;
        conn.execute("DELETE FROM config WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Get all config values as key-value pairs
    pub fn get_all_config(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached("SELECT key, value FROM config")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut config = std::collections::HashMap::new();
        for row in rows {
            let (key, value) = row?;
            config.insert(key, value);
        }
        Ok(config)
    }
}
