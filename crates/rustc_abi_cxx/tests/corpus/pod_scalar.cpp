// @target Foo
//
// Plain-old-data scalar aggregate. Verifies basic field placement and
// alignment for a POD-for-layout struct. Because Foo is POD, Clang sets
// `dsize == sizeof` — no tail-padding reuse is available to any hypothetical
// derived class.

struct Foo {
    int x;
    char y;
};

Foo g_foo;
