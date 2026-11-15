//! The schema, as an ordered list of migrations.

use rusqlite_migration::{M, Migrations};

/// The schema version this build writes (the number of migrations).
pub const SCHEMA_VERSION: usize = 1;

/// Every migration, oldest first. Under P6, `0001_v1.sql` is edited in place until the
/// switch-over; after that, changes are new, additive files.
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("sql/0001_v1.sql"))])
}
