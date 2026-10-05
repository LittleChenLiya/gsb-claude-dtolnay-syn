//! AST 属性编辑计划（attribute edit plan）。
//!
//! 以**显式路径**（如 `a::f`）选中 `syn::File` 中的 item，对其**外层属性**
//! 做增删，并保证：
//!
//! - 只动目标 item 的外层属性；内部属性（`#![...]`）、函数体、item 顺序与
//!   无关 token 顺序全部保持原样；
//! - 应用前先校验整个计划：属性参数 token 必须能解析为合法外层属性；
//!   删除目标必须是合法的无参路径；同一路径+同一操作的**重复目标**直接拒绝；
//! - 路径解析时若同一层出现多个同名 item（如两个 `fn f`），判定为**歧义**并拒绝；
//! - `#[cfg_attr(...)]` 只按路径 `cfg_attr` 匹配，其括号内的文本**不会**被
//!   当成直接属性参与增删；
//! - 原子性：任一步失败即整体失败，原 `File` 不被修改（本函数只读原 AST，
//!   在克隆上应用，失败时丢弃克隆）；
//! - 成功时返回新 `File` 与每处编辑的**旧/新属性 span 对应表**。
//!
//! # 计划用法
//!
//! ```
//! use attr_edit::{apply_plan, Edit, Plan};
//! use quote::quote;
//!
//! let file = syn::parse_file("mod a { fn f() {} } mod b { fn f() {} }").unwrap();
//!
//! // 计划：只给模块 a 里的 f 加 #[inline]；删除 a::f 上直接的 allow。
//! let plan = Plan {
//!     edits: vec![
//!         Edit::add("a::f", quote!(#[inline])),
//!         Edit::remove("a::f", "allow"),
//!     ],
//! };
//!
//! let (new_file, report) = apply_plan(&file, &plan).unwrap();
//! assert_eq!(report.len(), 2);
//! // 原 File 不变，模块 b 的 f 不受影响。
//! let out = quote::quote!(#new_file).to_string();
//! assert!(out.contains("mod b"));
//! ```
//!
//! # 局部命令
//!
//! ```text
//! cargo test -p attr-edit             # 全部验收测试（含本 doctest）
//! cargo test -p attr-edit --test plan # 只跑集成验收测试
//! ```

use std::collections::HashSet;
use std::fmt;

use proc_macro2::{Ident, Span, TokenStream};
use syn::parse::Parser as _;
use syn::spanned::Spanned as _;
use syn::{Attribute, File, Item, Path};

/// 一份编辑计划：按顺序应用的一组编辑。
pub struct Plan {
    pub edits: Vec<Edit>,
}

/// 单条编辑：显式路径 + 操作。
pub struct Edit {
    /// item 路径段，如 `["a", "f"]` 表示 `a::f`。
    pub path: Vec<Ident>,
    pub op: Op,
}

/// 对外层属性的操作。
pub enum Op {
    /// 追加外层属性，token 形如 `#[inline]`（可一次多个）。
    Add(TokenStream),
    /// 删除路径匹配的外层属性（如 `allow`）。只匹配直接外层属性，
    /// 不深入 `cfg_attr` 等属性的参数内部。
    Remove(Path),
}

impl Edit {
    /// 以 `"a::f"` 形式的字符串路径构造一条新增属性的编辑。
    ///
    /// # Panics
    ///
    /// `path` 不是合法的 `a::b::c` 形式标识符路径时 panic。
    pub fn add(path: &str, attrs: TokenStream) -> Self {
        Edit {
            path: parse_item_path(path),
            op: Op::Add(attrs),
        }
    }

    /// 以 `"a::f"` 形式的字符串路径构造一条删除属性的编辑，
    /// `attr` 是要删除的属性路径，如 `"allow"`、`"doc"`。
    ///
    /// # Panics
    ///
    /// 参数不是合法路径时 panic；属性路径带参数等语义问题在
    /// [`apply_plan`] 校验阶段以错误返回。
    pub fn remove(path: &str, attr: &str) -> Self {
        Edit {
            path: parse_item_path(path),
            op: Op::Remove(
                syn::parse_str(attr)
                    .unwrap_or_else(|e| panic!("invalid attribute path `{attr}`: {e}")),
            ),
        }
    }
}

fn parse_item_path(path: &str) -> Vec<Ident> {
    path.split("::")
        .map(|seg| {
            syn::parse_str::<Ident>(seg)
                .unwrap_or_else(|e| panic!("invalid item path `{path}`: {e}"))
        })
        .collect()
}

/// 一处编辑的旧/新属性 span 对应。
///
/// - 新增：`old == None`，`new == Some(新属性 span)`；
/// - 删除：`old == Some(被删属性 span)`，`new == None`。
#[derive(Debug)]
pub struct SpanPair {
    pub old: Option<Span>,
    pub new: Option<Span>,
}

/// 一条编辑的应用结果。
#[derive(Debug)]
pub struct EditReport {
    /// 在 `Plan::edits` 中的下标。
    pub edit: usize,
    /// 目标路径的字符串形式，如 `a::f`。
    pub target: String,
    /// 本次编辑每一处属性变动的旧/新 span 对应。
    pub spans: Vec<SpanPair>,
}

/// 计划失败原因。任何失败都发生在原 `File` 被修改之前。
#[derive(Debug)]
pub enum PlanError {
    /// 路径为空。
    EmptyPath { edit: usize },
    /// 计划中重复出现同一目标（同路径 + 同操作）。
    DuplicateTarget { edit: usize, key: String },
    /// 新增属性的 token 无法解析为合法的外层属性。
    InvalidAttrTokens { edit: usize, message: String },
    /// 删除目标不是合法的无参属性路径。
    InvalidRemovePath { edit: usize, message: String },
    /// 路径解析不到任何 item。
    NotFound { edit: usize, path: String },
    /// 同一路径在同一层匹配到多个 item。
    Ambiguous { edit: usize, path: String, matches: usize },
    /// 路径指向的 item 不支持承载外层属性（如 `Item::Verbatim`），
    /// 或中间路径段不是内联模块。
    UnsupportedItem { edit: usize, path: String },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PlanError::EmptyPath { edit } => write!(f, "edit {edit}: empty item path"),
            PlanError::DuplicateTarget { edit, key } => {
                write!(f, "edit {edit}: duplicate target `{key}`")
            }
            PlanError::InvalidAttrTokens { edit, message } => {
                write!(f, "edit {edit}: invalid attribute tokens: {message}")
            }
            PlanError::InvalidRemovePath { edit, message } => {
                write!(f, "edit {edit}: invalid remove path: {message}")
            }
            PlanError::NotFound { edit, path } => write!(f, "edit {edit}: `{path}` not found"),
            PlanError::Ambiguous { edit, path, matches } => {
                write!(f, "edit {edit}: `{path}` is ambiguous ({matches} matches)")
            }
            PlanError::UnsupportedItem { edit, path } => {
                write!(f, "edit {edit}: `{path}` does not support outer attributes")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// 应用计划：成功返回 `(新 File, 每条编辑的报告)`；失败返回错误，
/// 原 `file` 保证不被修改。
pub fn apply_plan(file: &File, plan: &Plan) -> Result<(File, Vec<EditReport>), PlanError> {
    // 第一阶段：纯校验，不触碰 AST。
    let mut seen = HashSet::new();
    let mut parsed_adds: Vec<Option<Vec<Attribute>>> = Vec::with_capacity(plan.edits.len());
    for (i, edit) in plan.edits.iter().enumerate() {
        if edit.path.is_empty() {
            return Err(PlanError::EmptyPath { edit: i });
        }
        let target = join_idents(&edit.path);
        let op_key = match &edit.op {
            Op::Add(tokens) => {
                let attrs = parse_outer_attrs(tokens)
                    .map_err(|message| PlanError::InvalidAttrTokens { edit: i, message })?;
                parsed_adds.push(Some(attrs));
                format!("add:{tokens}")
            }
            Op::Remove(path) => {
                validate_remove_path(path)
                    .map_err(|message| PlanError::InvalidRemovePath { edit: i, message })?;
                parsed_adds.push(None);
                format!("remove:{}", join_path(path))
            }
        };
        let key = format!("{target}::{op_key}");
        if !seen.insert(key.clone()) {
            return Err(PlanError::DuplicateTarget { edit: i, key });
        }
    }

    // 第二阶段：在克隆上应用；任何失败都直接丢弃克隆，原 File 不变。
    let mut new_file = file.clone();
    let mut reports = Vec::with_capacity(plan.edits.len());
    for (i, (edit, parsed)) in plan.edits.iter().zip(parsed_adds).enumerate() {
        let target = join_idents(&edit.path);
        let item = resolve_item(&mut new_file.items, &edit.path, i, &target)?;
        let attrs = item_attrs_mut(item)
            .ok_or_else(|| PlanError::UnsupportedItem { edit: i, path: target.clone() })?;
        let mut spans = Vec::new();
        match (&edit.op, parsed) {
            (Op::Add(_), Some(new_attrs)) => {
                for attr in new_attrs {
                    spans.push(SpanPair {
                        old: None,
                        new: Some(attr.span()),
                    });
                    attrs.push(attr);
                }
            }
            (Op::Remove(remove), None) => {
                let mut removed = Vec::new();
                attrs.retain(|attr| {
                    // 只比较属性自身的路径；`#[cfg_attr(.., allow(..))]` 的
                    // 路径是 `cfg_attr`，其参数文本不会在这里被展开匹配。
                    if path_eq(attr.path(), remove) {
                        removed.push(attr.span());
                        false
                    } else {
                        true
                    }
                });
                spans.extend(removed.into_iter().map(|old| SpanPair {
                    old: Some(old),
                    new: None,
                }));
            }
            _ => unreachable!("validated in phase one"),
        }
        reports.push(EditReport {
            edit: i,
            target,
            spans,
        });
    }
    Ok((new_file, reports))
}

/// 解析新增属性 token：必须是至少一个完整的外层属性，且无残留 token。
fn parse_outer_attrs(tokens: &TokenStream) -> Result<Vec<Attribute>, String> {
    let attrs = Attribute::parse_outer
        .parse2(tokens.clone())
        .map_err(|e| e.to_string())?;
    if attrs.is_empty() {
        return Err("expected at least one outer attribute (`#[...]`)".to_owned());
    }
    Ok(attrs)
}

/// 删除目标必须是无前导 `::`、无参数的非空路径，如 `allow` 或 `rustfmt::skip`。
fn validate_remove_path(path: &Path) -> Result<(), String> {
    if path.leading_colon.is_some() {
        return Err("leading `::` is not allowed".to_owned());
    }
    if path.segments.is_empty() {
        return Err("empty path".to_owned());
    }
    for seg in &path.segments {
        if !seg.arguments.is_empty() {
            return Err(format!("segment `{}` must not have arguments", seg.ident));
        }
    }
    Ok(())
}

fn path_eq(a: &Path, b: &Path) -> bool {
    a.leading_colon.is_none()
        && b.leading_colon.is_none()
        && a.segments.len() == b.segments.len()
        && a.segments
            .iter()
            .zip(&b.segments)
            .all(|(x, y)| x.ident == y.ident && x.arguments.is_empty() && y.arguments.is_empty())
}

fn join_idents(path: &[Ident]) -> String {
    path.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn join_path(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// 按显式路径定位 item：中间段必须是内联模块（`mod a { ... }`），
/// 同层多个同名 item 视为歧义。
fn resolve_item<'a>(
    items: &'a mut [Item],
    path: &[Ident],
    edit: usize,
    full_path: &str,
) -> Result<&'a mut Item, PlanError> {
    let seg = &path[0];
    let matches = items
        .iter()
        .filter(|item| item_ident(item).is_some_and(|ident| ident == seg))
        .count();
    if matches == 0 {
        return Err(PlanError::NotFound {
            edit,
            path: full_path.to_owned(),
        });
    }
    if matches > 1 {
        return Err(PlanError::Ambiguous {
            edit,
            path: full_path.to_owned(),
            matches,
        });
    }
    let item = items
        .iter_mut()
        .find(|item| item_ident(item).is_some_and(|ident| ident == seg))
        .expect("exactly one match");
    if path.len() == 1 {
        return Ok(item);
    }
    match item {
        Item::Mod(item_mod) => match &mut item_mod.content {
            Some((_, content)) => resolve_item(content, &path[1..], edit, full_path),
            None => Err(PlanError::UnsupportedItem {
                edit,
                path: full_path.to_owned(),
            }),
        },
        _ => Err(PlanError::NotFound {
            edit,
            path: full_path.to_owned(),
        }),
    }
}

fn item_ident(item: &Item) -> Option<&Ident> {
    match item {
        Item::Const(item) => Some(&item.ident),
        Item::Enum(item) => Some(&item.ident),
        Item::ExternCrate(item) => Some(&item.ident),
        Item::Fn(item) => Some(&item.sig.ident),
        Item::Macro(item) => item.ident.as_ref(),
        Item::Mod(item) => Some(&item.ident),
        Item::Static(item) => Some(&item.ident),
        Item::Struct(item) => Some(&item.ident),
        Item::Trait(item) => Some(&item.ident),
        Item::TraitAlias(item) => Some(&item.ident),
        Item::Type(item) => Some(&item.ident),
        Item::Union(item) => Some(&item.ident),
        _ => None,
    }
}

fn item_attrs_mut(item: &mut Item) -> Option<&mut Vec<Attribute>> {
    match item {
        Item::Const(item) => Some(&mut item.attrs),
        Item::Enum(item) => Some(&mut item.attrs),
        Item::ExternCrate(item) => Some(&mut item.attrs),
        Item::Fn(item) => Some(&mut item.attrs),
        Item::ForeignMod(item) => Some(&mut item.attrs),
        Item::Impl(item) => Some(&mut item.attrs),
        Item::Macro(item) => Some(&mut item.attrs),
        Item::Mod(item) => Some(&mut item.attrs),
        Item::Static(item) => Some(&mut item.attrs),
        Item::Struct(item) => Some(&mut item.attrs),
        Item::Trait(item) => Some(&mut item.attrs),
        Item::TraitAlias(item) => Some(&mut item.attrs),
        Item::Type(item) => Some(&mut item.attrs),
        Item::Union(item) => Some(&mut item.attrs),
        Item::Use(item) => Some(&mut item.attrs),
        _ => None,
    }
}
