//! `cxx_importer` — lazy libclang → rustcc HIR lowering.
//!
//! See `docs/cxx_importer.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(dead_code)]

pub mod aliases;
mod annotations;
#[cfg(feature = "build")]
pub mod build;
#[cfg(feature = "cache")]
mod cache;
pub mod cxx_exception;
mod diagnostics;
pub mod enums;
mod driver;
pub mod free_fns;
pub mod static_data;
pub mod hpp;
#[cfg(feature = "libclang")]
pub mod import;
mod lower;
pub mod macros;
mod name_mapping;
mod resolve;
pub mod rust_bindings;
pub mod rust_forwarders;
pub mod rust_stubs;
pub mod shims;

pub use aliases::{AliasSet, TypeAlias};
pub use annotations::{load_sidecar, Annotation, AnnotationSet, SidecarSchema};
pub use cxx_exception::{
    decode as decode_cxx_raw_error, render_throws_shim_cpp, CxxException, CxxExceptionKind,
    CxxRawError, CXX_EXC_OK, CXX_EXC_STD, CXX_EXC_UNKNOWN, CXX_RAW_ERROR_HEADER,
};
pub use enums::{CxxEnumDef, CxxEnumVariant, EnumSet};
pub use free_fns::{FreeFnDef, FreeFnSet};
pub use static_data::{StaticDataDef, StaticDataSet};
pub use macros::{MacroConst, MacroSet, MacroValue};
pub use diagnostics::ImportError;
pub use driver::{Driver, HeaderGraph};
pub use lower::LoweredEntity;
pub use name_mapping::{
    disambiguate_overloads, map_class, map_method, map_namespace, rust_name_for_operator,
    OverloadEntry, SelfReceiver,
};
pub use resolve::{EntityKey, ResolvedCursor};

#[cfg(feature = "libclang")]
pub use import::{
    discover_template_instantiations, import_header,
    import_header_with_annotations, import_header_with_extras, ImportExtras,
};
