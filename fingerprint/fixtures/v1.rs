//! Alpha crate, interface version 1.
//!
//! This file is an AST fixture: it is parsed by `syn::parse_file` in tests
//! and by the `api-fingerprint` CLI. It is never compiled.

use std::fmt;

// External re-export: recorded as `unknown` (external-reexport).
pub use serde::Serialize;
// Internal alias: recorded as a known `use` entry.
pub use self::nested::helper;

macro_rules! make_fn {
    ($name:ident) => {
        pub fn $name() {}
    };
}
// Macro-generated interface: recorded as `unknown` (macro-generated).
make_fn!(generated);

/// Compute something. Doc comments are ignored by the fingerprint.
#[inline]
pub fn compute<'a, T: Clone + fmt::Debug>(input: &'a [T], scale: u32) -> Vec<T>
where
    T: 'a,
{
    input.iter().cloned().collect()
}

#[repr(C)]
#[derive(Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    secret: u32,
}

#[repr(u8)]
pub enum Kind {
    Alpha = 1,
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
    pub fn helper(x: u8) -> u8 {
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

pub struct Defaulted<T = u32> {
    pub value: T,
}

pub fn r#match(r#type: u32) -> u32 {
    r#type
}

impl Point {
    pub fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    fn private_method(&self) {}
}

// Out-of-line module: recorded as `unknown` (out-of-line-module).
pub mod ext;
