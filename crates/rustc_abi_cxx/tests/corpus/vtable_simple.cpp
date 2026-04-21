// @kind vtable
// @target Base
//
// Primary vtable for a single-inheritance polymorphic class with three
// virtual members: dtor, a non-const method, and a const method. Expected
// slots after offset_to_top + RTTI are D1, D0, render, area (in source
// declaration order for non-dtor methods).

struct Base {
    virtual ~Base();
    virtual void render();
    virtual int area() const;
};

Base::~Base() = default;
void Base::render() {}
int Base::area() const { return 0; }
