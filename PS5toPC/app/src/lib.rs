//! Core of PS5toPC: PS4 `\x7FCNT` / PS5 `\x7FFIH` packages, the PFS
//! filesystem inside them, and SELF -> ELF reconstruction.
//!
//! Rules: positional streaming reads (a 40 GB package is never
//! loaded into memory), no console keys, and malformed or encrypted input fails
//! loudly instead of producing a plausible but wrong file.

pub mod anyps5;
pub mod binary;
pub mod convert;
pub mod crypto;
pub mod download;
pub mod elf;
pub mod error;
pub mod history;
pub mod kyty;
pub mod library;
pub mod media;
pub mod meta;
pub mod pfs;
pub mod pkg;
pub mod uninstall;
pub mod update;

pub use error::{Error, Result};
