// @kind layout
// @target Bird
//
// Polymorphic derived class that adds a field. Verifies primary-base
// sharing of the vptr: Bird inherits Animal's vptr (no new vptr
// allocated) and its `wingspan` lands in Animal's tail padding at
// offset 12 (Animal's dsize), not offset 16 (Animal's sizeof).

struct Animal {
    virtual ~Animal();
    int id;
};

struct Bird : Animal {
    int wingspan;
};

Animal::~Animal() = default;
Bird g_bird;
