//! Fixture crate root, interface version 1.
//!
//! Parsed only as an AST fixture by the api-fingerprint tests; never compiled.

use std::collections::HashMap;

pub mod nested {
    pub mod inner {
        /// Greets the caller. Docs are ignored by the fingerprint.
        pub fn greet<'a>(name: &'a str) -> &'a str {
            let trimmed = name.trim();
            if trimmed.is_empty() { "anonymous" } else { trimmed }
        }

        /// A 2-D point.
        pub struct Point {
            pub x: f64,
            pub y: f64,
        }

        #[repr(C)]
        pub struct Header {
            pub len: u32,
            pub flags: u8,
        }

        pub enum Color {
            Red = 1,
            Green = 2,
            Blue = 3,
        }

        pub trait Draw {
            fn draw(&self);
            fn bounding_box(&self) -> (f64, f64) {
                (0.0, 0.0)
            }
        }

        impl Point {
            pub fn origin() -> Point {
                Point { x: 0.0, y: 0.0 }
            }
        }
    }

    fn private_helper() -> u8 {
        1
    }
}

pub fn generic_default<T = u32>(value: T) -> T {
    value
}

pub fn constrained<T>(input: T) -> T
where
    T: Clone,
{
    input.clone()
}

pub fn r#match(raw: &str) -> bool {
    raw == "match"
}

pub fn two_lifetimes<'a, 'b>(x: &'a str, y: &'b str) -> &'a str {
    let _ = y;
    x
}

pub use std::fmt::Debug;
pub use self::nested::inner::Point as Point2D;
pub use std::io::*;

pub mod macros {
    some_proc_macro! {
        pub fn generated() {}
    }
}

pub mod unloaded;

mod private_mod {
    pub fn hidden() -> u8 {
        7
    }
}
