#![feature(rustc_attrs)]
#![allow(dead_code)]

//! Virtual-method override across single inheritance.
//!
//! `Dog::speak` replaces `Animal::speak` at the same vtable slot
//! — so calling `speak()` through an `Animal*` that actually points
//! at a `Dog` dispatches into `Dog::speak`, not `Animal::speak`.
//! `Dog` also adds a brand-new virtual `wag`, which lands in a
//! fresh vtable slot appended after the inherited ones.

pub class Animal {
    tag: u32,

    #[constructor]
    pub fn new(tag: u32) -> Self { Animal { tag } }

    #[cpp_virtual]
    pub fn speak(&self) -> u32 { self.tag * 10 }

    #[cpp_virtual]
    pub fn legs(&self) -> u32 { 4 }
}

pub class Dog : Animal {
    bark: u32,

    #[constructor]
    pub fn new(tag: u32, bark: u32) -> Self {
        Dog { __base: Animal::new(tag), bark }
    }

    // Same name as Animal::speak → OVERRIDE (takes over the
    // base's vtable slot).
    #[cpp_virtual]
    pub fn speak(&self) -> u32 { self.bark + 1000 }

    // New virtual, not in Animal — appended as a new slot.
    #[cpp_virtual]
    pub fn wag(&self) -> u32 { self.bark * 2 }
}

#[unsafe(no_mangle)]
pub extern "C" fn make_dog(tag: u32, bark: u32) -> *mut Dog {
    Box::into_raw(Box::new(Dog::new(tag, bark)))
}
#[unsafe(no_mangle)]
pub extern "C" fn make_animal(tag: u32) -> *mut Animal {
    Box::into_raw(Box::new(Animal::new(tag)))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_dog(d: *mut Dog) {
    if !d.is_null() { unsafe { drop(Box::from_raw(d)); } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_animal(a: *mut Animal) {
    if !a.is_null() { unsafe { drop(Box::from_raw(a)); } }
}
