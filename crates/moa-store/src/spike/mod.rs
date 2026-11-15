//! The spike's SQLite schema, frozen as SQL text.
//!
//! This is what the prototype's DB looks like once its Python setup has run. v1 imports
//! from it and exports back to it, and the test fixtures build brains from it. A test in
//! `moa-testkit` checks it against the spike's own files for as long as those exist.

/// The spike's tables and index, in the order its setup creates them.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// The spike's nine activity triggers.
pub const TRIGGERS_SQL: &str = include_str!("triggers.sql");
