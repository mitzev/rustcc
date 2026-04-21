// @target EmptyDerived
//
// Empty Base Optimization. `Empty` has sizeof=1 standalone but contributes
// zero bytes to `EmptyDerived`: `int x` lands at offset 0.

struct Empty {};

struct EmptyDerived : Empty {
    int x;
};

EmptyDerived g_ed;
