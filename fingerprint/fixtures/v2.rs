//! Alpha crate, interface version 2.
//!
//! Same shape as `v1.rs`, but with real interface changes:
//!   * `compute`: parameter type `u32` -> `u64`, extra `where` bound
//!   * `Point`: `#[repr(C)]` -> `#[repr(C, align(8))]`
//!   * `Kind::Alpha`: discriminant 1 -> 7
//!   * `Defaulted<T = u32>` -> `Defaulted<T = u64>`
//!   * `nested::helper`: parameter `u8` -> `u16`
//!   * `r#match` removed, `new_api` added

use std::fmt;

pub use serde::Serialize;
pub use self::nested::helper;

macro_rules! make_fn {
    ($name:ident) => {
        pub fn $name() {}
    };
}
make_fn!(generated);

/// Compute something.
#[inline]
pub fn compute<'a, T: Clone + fmt::Debug>(input: &'a [T], scale: u64) -> Vec<T>
where
    T: 'a + Send,
{
    input.iter().cloned().collect()
}

#[repr(C, align(8))]
#[derive(Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    secret: u32,
}

#[repr(u8)]
pub enum Kind {
    Alpha = 7,
    Beta { tag: String } = 2,
    Gamma = 4,
}

pub trait Store<'a> {
    type Item;
    fn get(&'a self, key: &str) -> Option<Self::Item>;
    fn put(&'a mut self, key: &str, value: Self::Item) {
        let _ = (key, value);
    }
}

pub mod nested {
    pub fn helper(x: u16) -> u16 {
        x + 1
    }

    pub mod deep {
        pub struct Leaf(pub u8);
    }

    fn private_helper() -> u8 {
        1
    }
}

mod private {
    pub fn not_exported() {}
}

pub struct Defaulted<T = u64> {
    pub value: T,
}

impl Point {
    pub fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    fn private_method(&self) {}
}

pub fn new_api(flag: bool) -> bool {
    flag
}

pub mod ext;
