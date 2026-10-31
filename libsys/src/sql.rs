//! A database of the backend (`sql:NAME` in the namespace; ruos serve's
//! backend.rs): SQLite by default, or PostgreSQL. SQL with parameters
//! (`?1`, `?2`… in SQLite) goes to the server through the store; the answer
//! is JSON: `{"columns": […], "rows": [[…]]}` for a query, `{"changed": N,
//! "last_id": ID}` otherwise.
use alloc::format;
use alloc::string::String;

use crate::store::{SQL, Store};
use crate::{Channel, Result, ns};

const OK: u8 = 0;

#[derive(Clone)]
pub struct Sql {
    store: Store,
    name: String,
}

impl Sql {
    /// `sql:NAME` from the namespace.
    pub fn get(name: &str) -> Option<Sql> {
        ns::get(&format!("sql:{name}")).map(|h| Sql { store: Store(Channel(h)), name: String::from(name) })
    }

    /// Any store client that may use `sql:NAME` (the shell's backend).
    pub fn with(store: Store, name: &str) -> Sql {
        Sql { store, name: String::from(name) }
    }

    /// `params`: a JSON array (`[]`, `["ann", 3]`). Err(why) when the
    /// database said no.
    pub fn query(&self, sql: &str, params: &str) -> Result<core::result::Result<String, String>> {
        self.call(sql, params, "json")
    }

    /// The answer as a text table.
    pub fn query_text(&self, sql: &str) -> Result<core::result::Result<String, String>> {
        self.call(sql, "", "text")
    }

    fn call(&self, sql: &str, params: &str, format: &str) -> Result<core::result::Result<String, String>> {
        let (st, b) = self.store.raw(SQL, &[self.name.as_bytes(), sql.as_bytes(), params.as_bytes(), format.as_bytes()])?;
        let t = String::from_utf8_lossy(&b).into_owned();
        Ok(if st == OK { Ok(t) } else { Err(t) })
    }
}

/// `s` as a JSON string (for `params`).
pub fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            '\n' => o += "\\n",
            '\r' => o += "\\r",
            '\t' => o += "\\t",
            c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
