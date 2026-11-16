//! `moa sql`: a read-only SQL shell on a brain, in-process (no `sqlite3` needed). It opens the
//! database read-only, so it works while the daemon runs, and it can't change anything.

use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};

use clap::Args;
use moa_store::{Store, StoreError};
use rusqlite::{Connection, types::ValueRef};

use crate::ctl::{Here, pick_brain};

#[derive(Args)]
pub struct SqlArgs {
    /// Which brain; the default is the one this folder and account belong to.
    #[arg(long)]
    brain: Option<String>,
    /// One statement to run; without it, one statement per line is read from stdin.
    sql: Option<String>,
}

pub fn sql(args: &SqlArgs) -> Result<(), String> {
    let here = Here::new()?;
    let config = here.read_config()?;
    let brain = pick_brain(&here, &config, args.brain.as_deref(), false)?;
    let store = Store::open_read_only(&here.paths.db(brain.as_str())).map_err(|e| match e {
        StoreError::Missing(_) => format!("the {brain} brain has no database yet"),
        other => other.to_string(),
    })?;
    let conn = store.conn();
    conn.pragma_update(None, "query_only", true)
        .map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout().lock();
    if let Some(statement) = &args.sql {
        return run(conn, statement, &mut stdout);
    }
    let stdin = std::io::stdin();
    let prompt = stdin.is_terminal();
    let mut lines = stdin.lock().lines();
    loop {
        if prompt {
            let _ = write!(stdout, "moa> ");
            let _ = stdout.flush();
        }
        let Some(Ok(line)) = lines.next() else {
            return Ok(());
        };
        if line.trim().is_empty() {
            continue;
        }
        // A bad line is reported and the next one still runs.
        if let Err(message) = run(conn, &line, &mut stdout) {
            let _ = stdout.flush();
            eprintln!("{message}");
        }
    }
}

/// Runs one statement and prints a header and its rows, tab-separated, NULL as empty.
fn run(conn: &Connection, statement: &str, out: &mut impl Write) -> Result<(), String> {
    let mut stmt = conn.prepare(statement).map_err(|e| e.to_string())?;
    let columns = stmt.column_count();
    if columns > 0 {
        let names: Vec<&str> = stmt.column_names().collect();
        let _ = writeln!(out, "{}", names.join("\t"));
    }
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut line = String::new();
        for i in 0..columns {
            if i > 0 {
                line.push('\t');
            }
            match row.get_ref(i).map_err(|e| e.to_string())? {
                ValueRef::Null => {}
                ValueRef::Integer(n) => {
                    let _ = write!(line, "{n}");
                }
                ValueRef::Real(f) => {
                    let _ = write!(line, "{f}");
                }
                ValueRef::Text(t) => line.push_str(&String::from_utf8_lossy(t)),
                // A blob is one line of hex, as SQLite writes a blob literal: x'0aff'.
                ValueRef::Blob(b) => {
                    line.push_str("x'");
                    for byte in b {
                        let _ = write!(line, "{byte:02x}");
                    }
                    line.push('\'');
                }
            }
        }
        let _ = writeln!(out, "{line}");
    }
    Ok(())
}
