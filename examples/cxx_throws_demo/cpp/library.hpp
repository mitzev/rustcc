// Demo C++ library exercising the rustcc::cxx_throws annotation
// across all three flavors:
//
//   - free fn with no throws    — for control (no Result wrapping)
//   - free fn with cxx_throws    — catch-all path
//   - free fn with typed catches — per-type dispatch
//   - class method (instance + static)
//   - constructor with throws    — Result<Self, _> shape
//
// Compile-link-run with `cargo run -p cxx_throws_demo`. The
// build.rs uses `cxx_importer::build::Build` to generate the
// Rust bindings + matching C++ shims; cargo links everything.
//
// Works on stock rustc — the throws path uses `extern "C"`
// shims (NOT `extern "C++"`), so no fork rustc is required.

#pragma once
#include <stdexcept>

// Two user-defined exception classes the typed-catch path
// discriminates between. Both are marked `rustcc::skip` —
// Rust doesn't need a binding for them; they exist only on
// the C++ side to be thrown + matched by the typed shim's
// `catch (const DomainError&)` arm. Without `skip`, the
// bindings emitter would generate `extern "C++"` decls for
// their ctors + what() methods, which requires fork rustc.
class [[clang::annotate("rustcc::skip")]] DomainError : public std::exception {
public:
    DomainError(const char* msg) : message_(msg) {}
    const char* what() const noexcept override { return message_; }
private:
    const char* message_;
};

class [[clang::annotate("rustcc::skip")]] RangeError : public std::exception {
public:
    RangeError(const char* msg) : message_(msg) {}
    const char* what() const noexcept override { return message_; }
private:
    const char* message_;
};

// A free fn that never actually throws — but we mark it
// `cxx_throws` anyway so this entire demo compiles on stock
// rustc. Without the annotation, the generated bindings put
// `add` in an `extern "C++"` block which only the fork rustc
// understands. Real users keep their non-throwing functions
// unannotated and rely on fork rustc; this demo's narrow
// constraint is "must run on default toolchain".
[[clang::annotate("rustcc::cxx_throws")]]
int add(int a, int b);

// Throws std::runtime_error on divide-by-zero. Catch-all
// `cxx_throws` puts the runtime_error message in
// CxxException::what().
[[clang::annotate("rustcc::cxx_throws")]]
int do_divide(int a, int b);

// Typed catches: DomainError vs RangeError get separate kind
// tags (CXX_EXC_TYPED_BASE+0 / +1), surfacing as
// CxxException::is_typed_at(0) / is_typed_at(1) on the Rust
// side.
[[clang::annotate("rustcc::cxx_throws(DomainError, RangeError)")]]
int compute(int selector);

// Class with throwing methods + a throwing constructor. Every
// method is annotated for the stock-rustc-friendliness reason
// above.
class Calc {
public:
    [[clang::annotate("rustcc::cxx_throws")]]
    Calc(int seed);          // throws DomainError if seed < 0

    [[clang::annotate("rustcc::cxx_throws")]]
    int read() const;        // never actually throws

    [[clang::annotate("rustcc::cxx_throws")]]
    int divide(int divisor); // throws std::runtime_error on /0

private:
    int seed_;
};
