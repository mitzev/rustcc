// @kind vtable
// @target Middle
//
// Intermediate-class vtable in a three-level chain. Exercises the
// algorithm's handling of a class that is neither root nor leaf: it
// both overrides an inherited slot (Root::a → Middle::a) and introduces
// a new virtual slot (b). The dtor pair must resolve to Middle (the
// most-derived class at query time), not Root.

struct Root {
    virtual ~Root();
    virtual void a();
};

struct Middle : Root {
    void a() override;
    virtual void b();
};

Root::~Root() = default;
void Root::a() {}
void Middle::a() {}
void Middle::b() {}

Middle g_middle;
