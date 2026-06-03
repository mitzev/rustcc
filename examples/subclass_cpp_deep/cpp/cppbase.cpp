#include "cppbase.hpp"

int g_shape_ctor = 0;
int g_shape_dtor = 0;

Shape::Shape(int x_) : x(x_) { g_shape_ctor++; }
Shape::~Shape() { g_shape_dtor++; }
int Shape::area() { return -1; }            // overridden in Rust

int Drawable::z_order() { return x; }       // key fn anchors Drawable RTTI
int Widget::handle() { return x; }          // key fn anchors Widget RTTI

Drawable::Drawable(int x_) : Shape(x_) {}
Widget::Widget(int x_) : Drawable(x_) {}
