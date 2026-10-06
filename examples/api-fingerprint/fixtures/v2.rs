//! Fixture crate root, interface version 2.

use std::collections::HashMap;
use std::fmt::Write as _;

pub mod nested {
    pub mod inner {
        // Body changed, lifetime renamed, whitespace changed: same digest.
        pub fn greet<'n>(name: &'n str) -> &'n str {
            let owned = name.to_uppercase();
            Box::leak(owned.into_boxed_str())
        }

        /// A 2-D point. Field type changed: digest changes.
        pub struct Point {
            pub x: f64,
            pub y: f32,
        }

        #[repr(C, packed)]
        pub struct Header {
            pub len: u32,
            pub flags: u8,
        }

        pub enum Color {
            Red = 1,
            Green = 2,
            Blue = 4,
        }

        pub trait Draw {
            fn draw(&self);
            fn bounding_box(&self) -> (f64, f64) {
                (1.0, 1.0)
            }
        }

        impl Point {
            pub fn origin() -> Point {
                Point { x: 1.0, y: 1.0 }
            }
        }
    }

    fn private_helper() -> u8 {
        42
    }
}

pub fn generic_default<T = u64>(value: T) -> T {
    value
}

pub fn constrained<T>(input: T) -> T
where
    T: Clone + Send,
{
    input.clone()
}

pub fn r#match(raw: &str) -> bool {
    raw != "match"
}

pub fn two_lifetimes<'x, 'y>(x: &'x str, y: &'y str) -> &'x str {
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
        99
    }
}
