pub mod circuit_breaker;
pub mod grpc_greeter;
pub mod in_memory_repo;

// The Diesel plumbing is written against one connection type, which only exists
// when a relational backend is on. `kodegen add mongodb` takes the same
// datastore slot with a driver that is not Diesel, so it turns `relational` off
// — these two files stay on disk (undo is `default = ["sqlite"]` again) but must
// not be compiled while no backend is selected.
#[cfg(feature = "relational")]
pub mod diesel_db;
#[cfg(feature = "relational")]
pub mod diesel_repo;
