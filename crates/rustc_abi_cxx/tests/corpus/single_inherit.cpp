// @target Derived
//
// Single non-virtual inheritance with tail-padding reuse. Base's
// user-declared destructor makes it non-POD-for-layout, which enables the
// derived class to place its first field (`z`) into Base's tail padding at
// offset 5 (Base::dsize). Without the dtor, Base would be POD and `z`
// would land at offset 8.

struct Base {
    int x;
    char y;
    ~Base() {}
};

struct Derived : Base {
    char z;
    int w;
};

Derived g_derived;
