// MSVC funclet smoke test: a throwing C++ function the Rust
// side will catch via P09.67's catch_switch + catch_pad.
//
// Compiled by run_msvc_runtime.sh into maybe_throws.obj
// using clang-cl targeting x86_64-pc-windows-msvc.
//
// `extern "C"` on MSVC implies `__declspec(nothrow)` by
// default, which makes the SEH machinery skip unwind table
// emission for this function — any thrown exception then
// calls `terminate()` instead of propagating. The fix is
// `noexcept(false)` (explicitly opt back into unwind tables);
// the `extern "C"` linkage is still needed so the symbol
// keeps its plain C name for the Rust side to bind to.

#include <stdexcept>
#include <cstdint>

extern "C" __declspec(dllexport) int32_t maybe_throws(int32_t x) noexcept(false) {
    if (x < 0) {
        throw std::runtime_error("negative input");
    }
    return x * 2;
}

// P09.68-msvc: typed-catch smoke fixture. Throws different
// types based on input so Rust-side typed catches can be
// validated against the MSVC RTTI dispatch.
class DomainError {
public:
    DomainError() {}
};

class RangeError {
public:
    RangeError() {}
};

extern "C" __declspec(dllexport) int32_t maybe_throws_typed(int32_t x) noexcept(false) {
    if (x == -1) {
        throw DomainError{};
    }
    if (x == -2) {
        throw RangeError{};
    }
    return x * 2;
}
