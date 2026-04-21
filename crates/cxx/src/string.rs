//! Bridge type for `std::string`.
//!
//! Opaque to Rust — size/alignment match the target stdlib's `std::string`;
//! rustcc codegen fills in the storage dimensions from `rustc_abi_cxx`.
//!
//! # Accessor contract
//!
//! `as_str`, `len`, `is_empty` require the rustc fork to emit calls to
//! the following extern "C" shims (Itanium ABI entry points plumbed
//! through `cxx_importer::shims`):
//!
//! | method         | emitted shim                                                      |
//! |----------------|-------------------------------------------------------------------|
//! | `len`          | `__rustcc_shim__ZNKSs4sizeEv(const std::string*) -> size_t`        |
//! | `is_empty`     | `__rustcc_shim__ZNKSs5emptyEv(const std::string*) -> bool`        |
//! | `as_str`       | `__rustcc_shim__ZNKSs4dataEv(const std::string*) -> const char*`   |
//!                 | paired with `len` for the slice length                            |
//!
//! Until the fork lands, invoking any of these panics with a message
//! that names the missing shim so the failure is self-describing. The
//! storage size (32 bytes) matches libstdc++ on x86_64-linux; libc++
//! differs and will require target-aware sizing once the `Target`
//! abstraction drives codegen.

pub struct CxxString {
    // Opaque storage; dimensions depend on libc++ vs libstdc++ and target.
    _storage: [u8; 32],
}

impl CxxString {
    /// Borrow the string bytes as a `&str`.
    ///
    /// Requires the codegen shim `__rustcc_shim__ZNKSs4dataEv` to be
    /// linked. UB if the underlying `std::string` contains non-UTF-8.
    pub fn as_str(&self) -> &str {
        unimplemented!(
            "CxxString::as_str requires rustcc codegen shim \
             __rustcc_shim__ZNKSs4dataEv (see crates/cxx/src/string.rs)"
        )
    }

    pub fn len(&self) -> usize {
        unimplemented!(
            "CxxString::len requires rustcc codegen shim \
             __rustcc_shim__ZNKSs4sizeEv (see crates/cxx/src/string.rs)"
        )
    }

    pub fn is_empty(&self) -> bool {
        unimplemented!(
            "CxxString::is_empty requires rustcc codegen shim \
             __rustcc_shim__ZNKSs5emptyEv (see crates/cxx/src/string.rs)"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::CxxString;

    /// libstdc++ `std::string` is 32 bytes on x86_64-linux. If this
    /// changes (e.g. porting to libc++, whose layout is 24 bytes), the
    /// Target-driven codegen needs to rewrite the storage dimension.
    #[test]
    fn storage_size_matches_libstdcxx_x86_64() {
        assert_eq!(core::mem::size_of::<CxxString>(), 32);
    }
}
