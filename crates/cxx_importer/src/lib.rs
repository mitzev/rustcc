//! `cxx_importer` — lazy libclang → rustcc HIR lowering.
//!
//! See `docs/cxx_importer.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(dead_code)]

mod annotations;
#[cfg(feature = "cache")]
mod cache;
mod diagnostics;
mod driver;
pub mod hpp;
#[cfg(feature = "libclang")]
pub mod import;
mod lower;
mod name_mapping;
mod resolve;
pub mod rust_bindings;
pub mod rust_forwarders;
pub mod rust_stubs;
pub mod shims;

pub use annotations::{Annotation, AnnotationSet, SidecarSchema};
pub use diagnostics::ImportError;
pub use driver::{Driver, HeaderGraph};
pub use lower::LoweredEntity;
pub use name_mapping::{
    disambiguate_overloads, map_class, map_method, map_namespace, rust_name_for_operator,
    OverloadEntry, SelfReceiver,
};
pub use resolve::{EntityKey, ResolvedCursor};

#[cfg(feature = "libclang")]
pub use import::{import_header, import_header_with_annotations};
