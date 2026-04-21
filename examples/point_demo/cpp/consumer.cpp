// C++ consumer that drives the Rust-authored Point/Segment types.
//
// Since the stubs abort on any method call, this binary exercises only
// the parts that do NOT go through Rust bodies: sizeof / alignof
// checks, stack placement via `unsigned char[]` fallback, etc. Once
// the rustc fork is live, replace the abort-ing stub object with the
// fork-emitted object and the same consumer will exercise real
// methods.

#include "point_demo-cxx.hpp"

#include <cstdio>
#include <cstdint>

int main() {
    // Layout assertions — resolved purely by the header + clang++,
    // no Rust body involvement. If these fail, the Rust-side layout
    // and the C++-side expectations disagreed.
    static_assert(sizeof(Point) == 8, "Point must be 8 bytes");
    static_assert(alignof(Point) == 4, "Point must align to 4");
    static_assert(sizeof(Segment) == 16, "Segment must be 16 bytes");
    static_assert(alignof(Segment) == 4, "Segment must align to 4");

    // Enum assertions — scoped enum with explicit discriminants.
    static_assert(sizeof(Orientation) == 4, "Orientation default is i32");
    static_assert(
        static_cast<int>(Orientation::East) == 90,
        "East discriminant preserved"
    );
    static_assert(
        static_cast<int>(Orientation::West) == 270,
        "West discriminant preserved"
    );

    std::fprintf(
        stdout,
        "sizeof(Point)=%zu alignof(Point)=%zu sizeof(Segment)=%zu sizeof(Orientation)=%zu\n",
        sizeof(Point),
        alignof(Point),
        sizeof(Segment),
        sizeof(Orientation)
    );
    return 0;
}
