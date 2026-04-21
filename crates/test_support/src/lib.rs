//! Shared harness for `rustc_abi_cxx` conformance testing.
//!
//! Provides:
//!
//! - A canonical golden-file format for record layouts, with a parser
//!   that round-trips with the renderer.
//! - A driver for `clang++ -Xclang -fdump-record-layouts` that parses
//!   Clang's textual dump into the same canonical form.
//!
//! Consumed by the `xtask refresh-goldens` subcommand (to regenerate
//! goldens from Clang) and by `rustc_abi_cxx`'s integration tests
//! (to diff our computed layouts against the goldens).
//!
//! See `docs/rustc_abi_cxx.md §10`.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod clang;
pub mod golden;
