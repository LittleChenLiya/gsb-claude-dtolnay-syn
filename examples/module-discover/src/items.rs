//! Helpers for extracting ranges and the `#[path]` attribute.

use proc_macro2::LineColumn;
use syn::{Attribute, Expr, ItemMod, Lit, Meta};

use crate::discover::LoadedFile;
use crate::model::{HopResolution, ModuleOrigin, SpanRange};

pub enum PathAttr {
    None,
    Literal(String),
    Unsupported,
}

/// Find the outer `#[path = ...]` attribute. Only string literals are
/// supported, matching the requirement to handle literal path attributes.
pub fn path_attr(attrs: &[Attribute]) -> PathAttr {
    for attr in attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        let Meta::NameValue(name_value) = &attr.meta else {
            return PathAttr::Unsupported;
        };
        match &name_value.value {
            Expr::Lit(expr_lit) => match &expr_lit.lit {
                Lit::Str(lit_str) => return PathAttr::Literal(lit_str.value()),
                _ => return PathAttr::Unsupported,
            },
            _ => return PathAttr::Unsupported,
        }
    }
    PathAttr::None
}

/// Byte range covering outer attributes and the whole `mod` item.
pub fn item_range(loaded: &LoadedFile, item_mod: &ItemMod) -> SpanRange {
    let start = match item_mod.attrs.first() {
        Some(attr) => attr.pound_token.span.start(),
        None => item_mod.mod_token.span.start(),
    };
    let end = if let Some((brace, _)) = &item_mod.content {
        brace.span.close().end()
    } else {
        item_mod
            .semi
            .as_ref()
            .expect("file module has semicolon")
            .span
            .end()
    };
    range(loaded, start, end)
}

pub fn range(loaded: &LoadedFile, start: LineColumn, end: LineColumn) -> SpanRange {
    loaded.line_map.range(&loaded.contents, start, end)
}

pub fn resolution_label(origin: &crate::model::ModuleOrigin) -> Option<HopResolution> {
    match origin {
        ModuleOrigin::Root => None,
        ModuleOrigin::Inline { .. } => Some(HopResolution::Inline),
        ModuleOrigin::File { path, .. } => Some(HopResolution::File(path.clone())),
        ModuleOrigin::Ambiguous { candidates, .. } => {
            Some(HopResolution::Ambiguous(candidates.clone()))
        }
        ModuleOrigin::Missing { .. } => Some(HopResolution::Missing),
        ModuleOrigin::OutsideRoot { path, resolved, .. } => Some(HopResolution::OutsideRoot {
            path: path.clone(),
            resolved: resolved.clone(),
        }),
        ModuleOrigin::Cycle { path, .. } => Some(HopResolution::Cycle(path.clone())),
        ModuleOrigin::Unreadable { path, .. } => Some(HopResolution::Unreadable(path.clone())),
        ModuleOrigin::NotUtf8 { path, .. } => Some(HopResolution::NotUtf8(path.clone())),
        ModuleOrigin::ParseError { path, .. } => Some(HopResolution::ParseError(path.clone())),
        ModuleOrigin::UnsupportedPath { .. } => Some(HopResolution::UnsupportedPath),
    }
}
