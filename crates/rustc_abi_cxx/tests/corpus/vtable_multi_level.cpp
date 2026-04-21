// @kind vtable
// @target Leaf
//
// Three-level inheritance chain where each level overrides a subset of
// the virtual methods it inherits, plus adds new ones. Leaf's vtable
// contains: Leaf's dtor pair, Leaf's `a` (overridden again), Leaf's `b`
// (overridden at Leaf), Leaf's `c` (new). Middle's overrides don't
// show up — final-overrider resolution bypasses them.

struct Root {
    virtual ~Root();
    virtual void a();
};

struct Middle : Root {
    void a() override;
    virtual void b();
};

struct Leaf : Middle {
    void a() override;
    void b() override;
    virtual void c();
};

Root::~Root() = default;
void Root::a() {}
void Middle::a() {}
void Middle::b() {}
void Leaf::a() {}
void Leaf::b() {}
void Leaf::c() {}
