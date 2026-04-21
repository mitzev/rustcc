//! Error types for layout queries.
//!
//! See `docs/rustc_abi_cxx.md §9`.

use std::error::Error;
use std::fmt;

use crate::ty::{ClassId, FieldId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    RecursiveValueMember { class: ClassId, field: FieldId },
    VirtualBaseUnsupported { class: ClassId, base: ClassId },
    MultipleBasesUnsupported { class: ClassId },
    UnsizedField { class: ClassId, field: FieldId },
    AlignmentOverflow { class: ClassId },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for LayoutError {}
