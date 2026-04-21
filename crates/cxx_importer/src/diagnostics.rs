//! Importer diagnostics.
//!
//! See `docs/cxx_importer.md §10, §11`.

use std::error::Error;
use std::fmt;

#[derive(Debug, Clone)]
pub enum ImportError {
    ClangDiagnostic {
        file: String,
        line: u32,
        message: String,
    },
    UnsupportedFeature {
        what: &'static str,
        where_: String,
    },
    OdrConflict {
        name: String,
        lhs: String,
        rhs: String,
    },
    SidecarParse {
        path: String,
        message: String,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for ImportError {}
