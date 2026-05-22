// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Single-inheritance polymorphic class hierarchy. Validates:
// - Virtual method mangling (no slot difference from non-virtual at
//   the symbol level; the dispatch goes through the vtable).
// - Override mangling (Derived::f mangles the same as the override
//   would in a flat class).
// - Virtual destructor mangling: scalar deleting form `??_G` and
//   base form `??1`.
// - vftable symbol `??_7Class@@6B@`.

struct Base {
    virtual void f(int);
    virtual int g() const;
    virtual ~Base();
};

void Base::f(int x) { (void)x; }
int Base::g() const { return 0; }
Base::~Base() {}

struct Derived : Base {
    void f(int) override;
    int g() const override;
    virtual ~Derived();
};

void Derived::f(int) {}
int Derived::g() const { return 1; }
Derived::~Derived() {}
