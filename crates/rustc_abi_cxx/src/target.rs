//! Target ABI parameters that feed layout and mangling.
//!
//! See `docs/rustc_abi_cxx.md §8`.

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Target {
    pub triple: String,
    pub pointer_width_bits: u32,
    pub long_double: LongDoubleKind,
    pub wchar_t_signed: bool,
    pub wchar_t_width: u32,
    pub aarch64_darwin_quirks: bool,
    /// Which C++ ABI dialect the platform uses. `Itanium` covers Linux,
    /// macOS, FreeBSD, and Windows-via-mingw; `Msvc` covers
    /// `*-pc-windows-msvc` (and, in the future, native MSVC clang targets
    /// on Wine). The flavor decides which mangler, vtable layout, and
    /// record-layout backend is selected by the dispatcher in
    /// `mangle::dispatch` / `vtable::dispatch` / `layout::dispatch`.
    ///
    /// Added in v1.09.0 alongside the MSVC ABI implementation. Existing
    /// targets default to `AbiFlavor::Itanium` and that's the behavior
    /// pre-v1.09.0 callers see.
    pub abi_flavor: AbiFlavor,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LongDoubleKind {
    F64,
    F80,
    F128,
}

/// Which C++ ABI a target implements. Used as a routing key by the
/// mangler, vtable, and layout dispatchers.
///
/// The two flavors diverge at every layer:
/// - **Mangling.** Itanium spells `?N::f(int) const` as `_ZNK1N1fEi`;
///   MSVC spells it as `?f@N@@AEBHH@Z`. Substitution / back-reference
///   schemes are entirely different.
/// - **Vtable.** Itanium puts offset-to-top + RTTI before the function
///   slots; MSVC has no such header (the RTTI complete-object-locator
///   lives at a *negative* offset reachable via vftable[−1]). Virtual
///   inheritance uses a vbtable (separate from the vftable) under MSVC.
/// - **Record layout.** MSVC reuses tail padding only for fields, not
///   bases; the empty-base optimization is more restricted; the vbptr
///   has a fixed insertion point near the start of the class.
/// - **Exception handling.** Itanium uses `__cxa_*` + libunwind tables;
///   MSVC uses SEH funclets and `_CxxThrowException`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AbiFlavor {
    Itanium,
    Msvc,
}

impl Target {
    pub fn x86_64_unknown_linux_gnu() -> Self {
        Self {
            triple: String::from("x86_64-unknown-linux-gnu"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F80,
            wchar_t_signed: true,
            wchar_t_width: 32,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Itanium,
        }
    }

    pub fn x86_64_apple_darwin() -> Self {
        Self {
            triple: String::from("x86_64-apple-darwin"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F64,
            wchar_t_signed: true,
            wchar_t_width: 32,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Itanium,
        }
    }

    pub fn aarch64_unknown_linux_gnu() -> Self {
        Self {
            triple: String::from("aarch64-unknown-linux-gnu"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F128,
            wchar_t_signed: false,
            wchar_t_width: 32,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Itanium,
        }
    }

    pub fn aarch64_apple_darwin() -> Self {
        Self {
            triple: String::from("aarch64-apple-darwin"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F64,
            wchar_t_signed: false,
            wchar_t_width: 32,
            aarch64_darwin_quirks: true,
            abi_flavor: AbiFlavor::Itanium,
        }
    }

    /// Windows x86_64 with the MSVC C++ ABI. Differs from
    /// `x86_64-pc-windows-gnu` (which uses the Itanium ABI via mingw-w64)
    /// in every layer that this crate models: name mangling, vtable
    /// layout, record layout, and downstream exception lowering.
    ///
    /// `long double` on MSVC is 64-bit (matches `double`); `wchar_t`
    /// is 16-bit unsigned (UTF-16). Both diverge from the Unix targets
    /// and feed into the MSVC mangler's `_W` / `_T` / `_O` builtin codes.
    pub fn x86_64_pc_windows_msvc() -> Self {
        Self {
            triple: String::from("x86_64-pc-windows-msvc"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F64,
            wchar_t_signed: false,
            wchar_t_width: 16,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Msvc,
        }
    }

    /// Windows aarch64 with the MSVC C++ ABI. Same ABI rules as the
    /// x86_64 target above; differs only in pointer width (already 64)
    /// and downstream calling-convention details that this crate doesn't
    /// model directly.
    pub fn aarch64_pc_windows_msvc() -> Self {
        Self {
            triple: String::from("aarch64-pc-windows-msvc"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F64,
            wchar_t_signed: false,
            wchar_t_width: 16,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Msvc,
        }
    }

    /// Windows x86_64 with the mingw-w64 toolchain. Uses the Itanium ABI
    /// (with a few mingw-specific tweaks not yet modeled — left to the
    /// follow-up that exercises mingw end-to-end). Pointer width / long
    /// double / wchar_t match MSVC's choices because the OS-level type
    /// system is the same.
    ///
    /// Phase-1 stepping stone before the full MSVC target lands.
    pub fn x86_64_pc_windows_gnu() -> Self {
        Self {
            triple: String::from("x86_64-pc-windows-gnu"),
            pointer_width_bits: 64,
            long_double: LongDoubleKind::F64,
            wchar_t_signed: false,
            wchar_t_width: 16,
            aarch64_darwin_quirks: false,
            abi_flavor: AbiFlavor::Itanium,
        }
    }

    /// Pick a target matching the host the driver is running on.
    /// Cross-compilation is out of scope for v1
    /// (docs/build_integration.md §9), so host-based selection is the
    /// right default for every invocation rustcc fields.
    pub fn host() -> Self {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Self::x86_64_unknown_linux_gnu()
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            Self::aarch64_unknown_linux_gnu()
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Self::x86_64_apple_darwin()
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Self::aarch64_apple_darwin()
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )))]
        {
            compile_error!(
                "rustcc host target not supported; add a Target constructor \
                 and extend Target::host() to cover this platform."
            )
        }
    }
}
