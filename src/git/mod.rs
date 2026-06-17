//! The `git` module: owns the Git Worker thread and all git2 interactions.
//!
//! It never imports from `ui`; the two modules only meet in `app.rs` via the
//! [`GitCommand`] / [`GitEvent`] message types defined here.

mod types;
mod worker;

pub use types::*;
pub use worker::run;
