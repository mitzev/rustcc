// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Operator overloads under the MSVC ABI. Operator names are MSVC's
// special two-character codes (??H = +, ??G = -, etc.) in the
// position normally occupied by the source identifier.

struct Vec {
    int x;
    Vec operator+(const Vec& other) const;
    Vec& operator=(const Vec& other);
    int operator[](int i);
    bool operator==(const Vec& other) const;
    bool operator<(const Vec& other) const;
};

Vec Vec::operator+(const Vec& other) const { return Vec{x + other.x}; }
Vec& Vec::operator=(const Vec& other) { x = other.x; return *this; }
int Vec::operator[](int i) { return x + i; }
bool Vec::operator==(const Vec& other) const { return x == other.x; }
bool Vec::operator<(const Vec& other) const { return x < other.x; }
