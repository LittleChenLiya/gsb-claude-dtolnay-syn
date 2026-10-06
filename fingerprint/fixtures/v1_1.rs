//! Alpha crate, version 1.1.
//!
//! Same public interface as `v1.rs`: only bodies, whitespace, comments,
//! lifetime *names* and private items changed. The SHA-256 digest of the
//! fingerprint must be identical to `v1.rs`.

use   std::fmt ;

pub use serde::Serialize;
pub use self::nested::helper;

macro_rules! make_fn {
    ($name:ident) => {
        pub fn $name() {}
    };
}
make_fn!(generated);

/// Compute something. (Doc comment text changed; still ignored.)
#[inline]
pub fn compute<'src, T: Clone + fmt::Debug>(input: &'src [T], scale: u32) -> Vec<T>
where
    T: 'src,
{
    // Body rewritten completely; the digest must not move.
    let mut out = Vec::with_capacity(input.len());
    out.extend_from_slice(input);
    out
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

pub trait Store<'storage> {
    type Item;
    fn get(&'storage self, key: &str) -> Option<Self::Item>;
    fn put(&'storage mut self, key: &str, value: Self::Item) {
        // Default body changed; the digest must not move.
        drop(key);
        drop(value);
    }
}

pub mod nested {
    pub fn helper(x: u8) -> u8 {
        // Body changed.
        x.wrapping_add(1)
    }

    pub mod deep {
        pub struct Leaf(pub u8);
    }

    fn private_helper() -> u8 {
        42 // private item changed; not part of the report
    }
}

mod private {
    pub fn not_exported() {
        // private module contents changed
        println!("hello");
    }
}

pub struct Defaulted<T = u32> {
    pub value: T,
}

pub fn r#match(r#type: u32) -> u32 {
    r#type
}

impl Point {
    pub fn norm(&self) -> f64 {
        self.x.hypot(self.y) // body changed
    }

    fn private_method(&self) {
        // private method changed
    }
}

pub mod ext;
