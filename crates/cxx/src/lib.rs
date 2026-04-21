//! User-facing runtime for rustcc C++ interop.
//!
//! Provides `CxxOwned`, `CxxBox`, `CxxShared`, `CxxMove`, `CxxBase`, the
//! `cxx_stack!` macro, and STL-bridge types. rustcc codegen replaces
//! method bodies and Drop glue with Itanium-ABI-aware calls at compile
//! time; the in-crate bodies are skeleton placeholders.
//!
//! See `docs/ownership_and_safety.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(dead_code)]

mod base;
mod boxed;
mod move_;
mod owned;
mod shared;
mod stack;
mod string;

pub use base::CxxBase;
pub use boxed::CxxBox;
pub use move_::CxxMove;
pub use owned::CxxOwned;
pub use shared::CxxShared;
pub use string::CxxString;

// `cxx_stack!` is exported at the crate root via #[macro_export].
