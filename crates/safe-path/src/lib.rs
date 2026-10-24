//! Names stored in an archive, made safe to create on disk.
//!
//! An archive says where each of its files goes, and it can lie: `../../x`,
//! `C:\Windows\x`, a symlink that points out of the folder and a file written
//! through it, a name with `:` that Windows reads as an alternate data stream
//! (CVE-2025-8088), a 1 KB entry that inflates to 100 GB. Rust's memory
//! safety does nothing about any of these; this crate is where they are
//! stopped, once, for every format.
//!
//! - [`sanitize`] turns a stored name into a [`RelPath`], or says why not.
//!   The same rules apply on every system, so an archive extracts the same
//!   way everywhere.
//! - [`Destination`] creates files under one folder and refuses to go
//!   through a link on the way, or to place a link that points out.
//! - [`Budget`] stops an extraction that writes more than it should.
//! - [`origin`] carries the "downloaded from the Internet" mark from the
//!   archive to what comes out of it.

mod budget;
mod destination;
mod name;
pub mod origin;

pub use budget::{Bomb, Budget, Limits, Metered};
pub use destination::{Destination, Unsafe};
#[cfg(feature = "path-walk")]
mod path_walk;
pub use name::{Change, Reason, RelPath, Verdict, sanitize};
