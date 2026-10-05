//! The `codoseo` binary's internals, as a library so integration tests can exercise the
//! worker loop directly instead of only through the compiled binary.

pub mod cli;
pub mod jobs;
pub mod scheduler;
pub mod worker;
