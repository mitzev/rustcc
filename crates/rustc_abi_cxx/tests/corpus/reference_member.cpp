// @kind layout
// @target HoldsRef
//
// A class with a reference member. Verifies that the layout algorithm
// treats `T&` as pointer-sized, pointer-aligned. HoldsRef remains POD-
// for-layout because it's aggregate + standard-layout + trivially-
// copyable (references have trivial copy-ctor; copy-assign is deleted
// but that doesn't disqualify trivial-copyability).

int g_value = 42;

struct HoldsRef {
    int& ref;
    int tag;
};

HoldsRef g_holds_ref{g_value, 0};
