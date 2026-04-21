// @kind layout
// @target Outer
//
// Field whose type is another record. Verifies that nested layouts
// compose: Inner's size/alignment feed Outer's placement. Because Inner
// is POD-for-layout, dsize == sizeof and its tail padding is not
// reusable inside Outer.

struct Inner {
    int a;
    char b;
};

struct Outer {
    char prefix;
    Inner inner;
    int trailing;
};

Outer g_outer;
