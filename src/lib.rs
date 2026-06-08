//! ferro as a library: the Frappe data-plane modules, shared by the pure-Rust `ferro`
//! binary and the Python-embedding `ferrod` binary.
//!
//! The data-plane modules (auth/crypto/meta/naming/orm/util) are identical to what the
//! `ferro` binary compiles; `ferrod` reuses them and adds an embedded CPython interpreter
//! (`pyrt`, `pydoc`, `hooks`) to run real Frappe app controllers on top.

pub mod auth;
pub mod crypto;
pub mod meta;
pub mod naming;
pub mod orm;
pub mod util;

// Embedded-Python layer (only compiled for ferrod, which links libpython).
#[cfg(feature = "python")]
pub mod pyrt;
