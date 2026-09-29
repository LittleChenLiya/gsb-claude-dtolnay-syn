//! Prints the result of consolidating a small built-in snippet.

use consolidate_use::consolidate;
use quote::ToTokens;

fn main() {
    let snippets = [
        r#"
use std::io::{self, Read};
use std::io::Write;
"#,
        r#"
use std::io::*;
use std::io::{self, self as Io, Read as _, Error as E};
"#,
        r#"
use ::std::io::{Read, Write};
use std::io::Error;
"#,
        r#"
mod m { use std::io::{Read, Write}; }
use core::fmt::Display;
"#,
        r#"
use a::b::{self as B1, self as B2, c as _, *};
"#,
        r#"
use self as root;
"#,
    ];

    for source in snippets {
        let file = syn::parse_str(source).expect("valid rust");
        let consolidated = consolidate(&file);
        for item in &consolidated.items {
            println!("{}", item.to_token_stream());
        }
        println!();
    }
}
