//! Importer diagnostics.
//!
//! See `docs/cxx_importer.md §10, §11`.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// A C++-side source location captured during libclang traversal.
/// Carried alongside [`ImportError`] variants so the diagnostic
/// formatter can point at the offending declaration with file +
/// line + column instead of just a stringified name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

impl SourceSpan {
    pub fn new(file: impl Into<PathBuf>, line: u32, column: u32) -> Self {
        Self {
            file: file.into(),
            line,
            column,
        }
    }
}

impl fmt::Display for SourceSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file.display(), self.line, self.column)
    }
}

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
        /// C++ source span of the unsupported declaration. `None`
        /// when the failure isn't tied to a specific entity (e.g.
        /// the libclang invocation itself failed before any cursor
        /// was visited).
        span: Option<SourceSpan>,
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

impl ImportError {
    /// Construct an `UnsupportedFeature` without a span — for call
    /// sites that don't have a cursor at hand. The diagnostic
    /// formatter falls back to the `where_` string in that case.
    pub fn unsupported(what: &'static str, where_: impl Into<String>) -> Self {
        Self::UnsupportedFeature {
            what,
            where_: where_.into(),
            span: None,
        }
    }

    /// Same as [`Self::unsupported`] but carries a C++ source span.
    pub fn unsupported_at(
        what: &'static str,
        where_: impl Into<String>,
        span: SourceSpan,
    ) -> Self {
        Self::UnsupportedFeature {
            what,
            where_: where_.into(),
            span: Some(span),
        }
    }

    /// The C++ source span attached to this error, if any.
    pub fn span(&self) -> Option<&SourceSpan> {
        match self {
            ImportError::UnsupportedFeature { span, .. } => span.as_ref(),
            _ => None,
        }
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Diagnostic::from_error(self).fmt(f)
    }
}

impl Error for ImportError {}

/// Rich-formatter façade over [`ImportError`]. Renders the error
/// with a `<file>:<line>:<col>` header (when a span is available)
/// and a one-line message, mirroring the rustc / clang diagnostic
/// shape. Each call produces a new render — the formatter is
/// stateless.
///
/// Per `docs/cxx_importer.md §10`, every lowering failure should
/// produce a diagnostic with two source locations: the C++
/// declaration and the Rust use site that triggered the import.
/// The Rust-side location requires an HIR-integration path that
/// today's standalone `build.rs` use case doesn't have, so v1 of
/// this formatter renders the C++ side only and leaves the
/// "triggered by …" note as a future-release line item.
#[derive(Debug)]
pub struct Diagnostic<'a> {
    error: &'a ImportError,
}

impl<'a> Diagnostic<'a> {
    pub fn from_error(error: &'a ImportError) -> Self {
        Self { error }
    }
}

impl fmt::Display for Diagnostic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.error {
            ImportError::ClangDiagnostic {
                file,
                line,
                message,
            } => {
                write!(f, "{file}:{line}: {message}")
            }
            ImportError::UnsupportedFeature {
                what,
                where_,
                span,
            } => {
                if let Some(s) = span {
                    write!(f, "{s}: cannot import `{where_}`: {what}")
                } else {
                    write!(f, "cannot import `{where_}`: {what}")
                }
            }
            ImportError::OdrConflict { name, lhs, rhs } => {
                write!(
                    f,
                    "ODR conflict for `{name}`: imported once as `{lhs}` and again as `{rhs}`"
                )
            }
            ImportError::SidecarParse { path, message } => {
                write!(f, "sidecar `{path}`: {message}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_renders_unsupported_with_span() {
        let err = ImportError::unsupported_at(
            "virtual inheritance not supported",
            "ns::Widget",
            SourceSpan::new("widget.hpp", 14, 7),
        );
        assert_eq!(
            format!("{err}"),
            "widget.hpp:14:7: cannot import `ns::Widget`: virtual inheritance not supported",
        );
    }

    #[test]
    fn diagnostic_renders_unsupported_without_span() {
        let err = ImportError::unsupported("template parameter pack", "ns::Pack");
        assert_eq!(
            format!("{err}"),
            "cannot import `ns::Pack`: template parameter pack",
        );
    }

    #[test]
    fn diagnostic_renders_clang_diagnostic_with_file_and_line() {
        let err = ImportError::ClangDiagnostic {
            file: "broken.hpp".into(),
            line: 42,
            message: "expected ';' before '}' token".into(),
        };
        assert_eq!(
            format!("{err}"),
            "broken.hpp:42: expected ';' before '}' token",
        );
    }
}
