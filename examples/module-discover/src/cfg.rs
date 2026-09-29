//! Parsing of `#[cfg(...)]` attributes into [`CfgExpr`].
//!
//! Nothing in here decides whether a predicate is enabled or disabled; the
//! expressions are preserved structurally for the caller to evaluate.

use proc_macro2::TokenStream;
use syn::punctuated::Punctuated;
use syn::{Attribute, Meta, Token};

use crate::model::{CfgExpr, CfgPredicate};

/// Extract and parse every `#[cfg(...)]` outer attribute, in source order.
pub fn parse_cfg_attrs(attrs: &[Attribute]) -> Vec<CfgExpr> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .filter_map(|attr| match &attr.meta {
            Meta::List(list) => Some(parse_tokens(list.tokens.clone())),
            _ => None,
        })
        .collect()
}

fn parse_tokens(tokens: TokenStream) -> CfgExpr {
    let fallback = tokens.to_string();
    parse_single(tokens).unwrap_or(CfgExpr::Unknown(fallback))
}

fn parse_single(tokens: TokenStream) -> syn::Result<CfgExpr> {
    syn::parse::Parser::parse2(
        |input: syn::parse::ParseStream| {
            // A bare comma-separated list is shorthand for all(...).
            let parts: Punctuated<CfgExpr, Token![,]> = Punctuated::parse_terminated(input)?;
            if parts.len() == 1 {
                Ok(parts.into_iter().next().unwrap())
            } else {
                Ok(CfgExpr::All(parts.into_iter().collect()))
            }
        },
        tokens,
    )
}

impl syn::parse::Parse for CfgExpr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        // Boolean literal.
        if input.peek(syn::LitBool) {
            let lit: syn::LitBool = input.parse()?;
            return Ok(CfgExpr::Bool(lit.value));
        }

        // Identifier-based predicate or combinator.
        let ident: syn::Ident = input.parse()?;
        if ident == "any" || ident == "all" || ident == "not" {
            let content;
            syn::parenthesized!(content in input);
            let parts: Punctuated<CfgExpr, Token![,]> = Punctuated::parse_terminated(&content)?;
            return match ident.to_string().as_str() {
                "not" => {
                    let mut iter = parts.into_iter();
                    let first = iter
                        .next()
                        .ok_or_else(|| content.error("cfg(not) requires one predicate"))?;
                    if iter.next().is_some() {
                        return Err(content.error("cfg(not) takes exactly one predicate"));
                    }
                    Ok(CfgExpr::Not(Box::new(first)))
                }
                "any" => Ok(CfgExpr::Any(parts.into_iter().collect())),
                _ => Ok(CfgExpr::All(parts.into_iter().collect())),
            };
        }

        // key = "value"
        if input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            let value = input.parse::<syn::LitStr>()?;
            return Ok(CfgExpr::Predicate(CfgPredicate::KeyValue {
                key: ident.to_string(),
                value: value.value(),
            }));
        }

        Ok(CfgExpr::Predicate(CfgPredicate::Flag(ident.to_string())))
    }
}
