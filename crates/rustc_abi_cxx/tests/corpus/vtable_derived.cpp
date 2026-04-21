// @kind vtable
// @target Derived
//
// Derived class overriding `render` but inheriting `area`. The vtable
// keeps the same slot layout as Base, but with `render`'s slot pointing
// at Derived's override and `area`'s slot still pointing at Base's
// implementation (final-overrider resolution). Dtors are Derived's.

struct Base {
    virtual ~Base();
    virtual void render();
    virtual int area() const;
};

struct Derived : Base {
    void render() override;
    int value;
};

Base::~Base() = default;
void Base::render() {}
int Base::area() const { return 0; }
void Derived::render() {}
