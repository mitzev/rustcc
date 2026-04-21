//! Lower Clang cursors into `rustc_abi_cxx` IR plus rustcc HIR stubs.
//!
//! See `docs/cxx_importer.md §6, §9`.

use rustc_abi_cxx::{ClassId, CxxTypeCtx};

use crate::diagnostics::ImportError;
use crate::driver::Driver;
use crate::resolve::ResolvedCursor;

pub enum LoweredEntity {
    Class(ClassId),
    Function(String),
    Namespace(String),
    Typedef(String),
}

impl Driver {
    pub fn lower(
        &self,
        _ctx: &mut CxxTypeCtx,
        _cursor: &ResolvedCursor,
    ) -> Result<LoweredEntity, ImportError> {
        todo!("cursor → IR — see docs §6, §9")
    }
}
