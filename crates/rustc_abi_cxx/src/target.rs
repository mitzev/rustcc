//! Target ABI parameters that feed layout and mangling.
//!
//! See `docs/rustc_abi_cxx.md §8`.

#[derive(Clone, Debug)]
pub struct Target {
    pub triple: String,
    pub pointer_width_bits: u32,
    pub long_double: LongDoubleKind,
    pub wchar_t_signed: bool,
    pub wchar_t_width: u32,
    pub aarch64_darwin_quirks: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LongDoubleKind {
    F64,
    F80,
    F128,
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
