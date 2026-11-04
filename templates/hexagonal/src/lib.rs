#![allow(clippy::double_must_use)]

pub mod api;
pub mod application;
pub mod bootstrap;
pub mod domain;
pub mod infrastructure;

// Cargo features are additive, so nothing else stops `cargo build --features
// seaorm-postgres` on a project whose `default` is still a Diesel backend: the
// two adapters are separate modules, so both would compile and the binary would
// link a C driver *and* sqlx while `bootstrap.rs` uses one of them. One ORM per
// project is a decision, so it is stated where the compiler can enforce it.
#[cfg(all(feature = "relational", feature = "seaorm"))]
compile_error!(
    "one ORM per project: `relational` (Diesel) and `seaorm` cannot both be on — switch with `kodegen add <db> --orm seaorm`"
);
