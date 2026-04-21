// @kind mangle
//
// Operator overload mangling. Exercises `pl`, `aS`, `ix` op codes and
// substitution of the receiver class type in a reference parameter.

struct Vec {
    int x;
    Vec operator+(const Vec& other) const;
    Vec& operator=(const Vec& other);
    int& operator[](int idx);
};

Vec Vec::operator+(const Vec& other) const {
    return Vec{x + other.x};
}

Vec& Vec::operator=(const Vec& other) {
    x = other.x;
    return *this;
}

int& Vec::operator[](int idx) {
    (void)idx;
    return x;
}
