//! 验收测试：AST 属性编辑计划。
//!
//! 运行：cargo test -p attr-edit --test plan

use std::str::FromStr as _;

use attr_edit::{apply_plan, Edit, Plan, PlanError};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{File, Item};

fn parse(src: &str) -> File {
    syn::parse_file(src).unwrap()
}

fn tokens_of(file: &File) -> String {
    quote!(#file).to_string()
}

/// 按路径取出 item 的属性（测试辅助）。
fn attrs_at<'a>(items: &'a [Item], path: &[&str]) -> &'a [syn::Attribute] {
    let (seg, rest) = path.split_first().unwrap();
    let mut found = items
        .iter()
        .filter(|item| item_name(item).as_deref() == Some(*seg));
    let item = found.next().unwrap();
    assert!(found.next().is_none(), "ambiguous fixture path");
    if rest.is_empty() {
        return item_attrs(item);
    }
    match item {
        Item::Mod(m) => attrs_at(&m.content.as_ref().unwrap().1, rest),
        _ => panic!("not a module"),
    }
}

fn item_name(item: &Item) -> Option<String> {
    match item {
        Item::Fn(i) => Some(i.sig.ident.to_string()),
        Item::Mod(i) => Some(i.ident.to_string()),
        Item::Struct(i) => Some(i.ident.to_string()),
        _ => None,
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Fn(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        _ => panic!("unsupported fixture item"),
    }
}

fn item_names(file: &File) -> Vec<String> {
    file.items.iter().filter_map(item_name).collect()
}

/// 验收点 1：只给模块 a 里的函数 f 加 #[inline]，另一个模块的 f 不变。
#[test]
fn add_inline_only_to_module_a_f() {
    let file = parse("mod a { fn f() {} } mod b { fn f() {} }");
    let before = tokens_of(&file);

    let plan = Plan {
        edits: vec![Edit::add("a::f", quote!(#[inline]))],
    };
    let (new_file, report) = apply_plan(&file, &plan).unwrap();

    // 原 File 不被修改。
    assert_eq!(tokens_of(&file), before);

    // a::f 带上了 inline，b::f 没有任何属性。
    let a_attrs = attrs_at(&new_file.items, &["a", "f"]);
    assert_eq!(a_attrs.len(), 1);
    assert!(a_attrs[0].path().is_ident("inline"));
    assert!(attrs_at(&new_file.items, &["b", "f"]).is_empty());

    // span 对应：一处新增，old 为 None、new 为 Some。
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].target, "a::f");
    assert_eq!(report[0].spans.len(), 1);
    assert!(report[0].spans[0].old.is_none());
    assert!(report[0].spans[0].new.is_some());

    // item 顺序不变。
    assert_eq!(item_names(&new_file), item_names(&file));
}

/// 验收点 2：删除 allow 不碰 cfg_attr 生成的 allow。
#[test]
fn remove_allow_keeps_cfg_attr_allow() {
    let file = parse("mod a { #[allow(unused)] #[cfg_attr(test, allow(dead_code))] fn f() {} }");
    // 记录被删 allow 原来的 span 位置（proc-macro2 开启了 span-locations）。
    let old_allow_start = attrs_at(&file.items, &["a", "f"])[0].pound_token.span.start();

    let plan = Plan {
        edits: vec![Edit::remove("a::f", "allow")],
    };
    let (new_file, report) = apply_plan(&file, &plan).unwrap();

    // 直接 allow 被删，cfg_attr 原样保留（含其参数里的 allow 文本）。
    let attrs = attrs_at(&new_file.items, &["a", "f"]);
    assert_eq!(attrs.len(), 1);
    assert!(attrs[0].path().is_ident("cfg_attr"));
    let cfg_attr_text = quote!(#(#attrs)*).to_string();
    assert!(cfg_attr_text.contains("allow"), "{cfg_attr_text}");

    // span 对应：一处删除，old 指向原 allow 的位置，new 为 None。
    assert_eq!(report[0].spans.len(), 1);
    let pair = &report[0].spans[0];
    assert_eq!(pair.old.unwrap().start(), old_allow_start);
    assert!(pair.new.is_none());
}

/// cfg_attr 里出现的 allow 不算直接属性：没有直接 allow 时删除为零处。
#[test]
fn cfg_attr_argument_is_not_a_direct_attribute() {
    let file = parse("mod a { #[cfg_attr(test, allow(dead_code))] fn f() {} }");
    let plan = Plan {
        edits: vec![Edit::remove("a::f", "allow")],
    };
    let (new_file, report) = apply_plan(&file, &plan).unwrap();
    assert_eq!(report[0].spans.len(), 0);
    assert_eq!(attrs_at(&new_file.items, &["a", "f"]).len(), 1);
}

/// 验收点 3：重复目标先拒绝，原 File 不变。
#[test]
fn duplicate_target_rejected() {
    let file = parse("mod a { fn f() {} }");
    let before = tokens_of(&file);
    let plan = Plan {
        edits: vec![
            Edit::add("a::f", quote!(#[inline])),
            Edit::add("a::f", quote!(#[inline])),
        ],
    };
    match apply_plan(&file, &plan) {
        Err(PlanError::DuplicateTarget { edit, .. }) => assert_eq!(edit, 1),
        Err(e) => panic!("expected DuplicateTarget, got {e}"),
        Ok(_) => panic!("expected DuplicateTarget, got Ok"),
    }
    assert_eq!(tokens_of(&file), before);
}

/// 同路径歧义：同一模块里两个同名 f，拒绝。
#[test]
fn ambiguous_path_rejected() {
    let file = parse("mod a { fn f() {} fn f() {} }");
    let plan = Plan {
        edits: vec![Edit::add("a::f", quote!(#[inline]))],
    };
    match apply_plan(&file, &plan) {
        Err(PlanError::Ambiguous { matches, .. }) => assert_eq!(matches, 2),
        Err(e) => panic!("expected Ambiguous, got {e}"),
        Ok(_) => panic!("expected Ambiguous, got Ok"),
    }
}

/// 同层两个同名模块也算歧义。
#[test]
fn ambiguous_module_rejected() {
    let file = parse("mod a { fn f() {} } mod a { fn g() {} }");
    let plan = Plan {
        edits: vec![Edit::add("a::f", quote!(#[inline]))],
    };
    assert!(matches!(
        apply_plan(&file, &plan),
        Err(PlanError::Ambiguous { .. })
    ));
}

/// 路径不存在：NotFound。
#[test]
fn missing_path_rejected() {
    let file = parse("mod a { fn f() {} }");
    let plan = Plan {
        edits: vec![Edit::remove("a::g", "allow")],
    };
    assert!(matches!(
        apply_plan(&file, &plan),
        Err(PlanError::NotFound { .. })
    ));
}

/// 属性参数解析检查：残留 token、内层属性都拒绝。
#[test]
fn invalid_attribute_tokens_rejected() {
    let file = parse("mod a { fn f() {} }");

    // 属性后面有多余 token。
    let trailing = TokenStream::from_str("#[allow(x)] junk").unwrap();
    let plan = Plan {
        edits: vec![Edit::add("a::f", trailing)],
    };
    assert!(matches!(
        apply_plan(&file, &plan),
        Err(PlanError::InvalidAttrTokens { .. })
    ));

    // 内层属性不是外层属性。
    let inner = TokenStream::from_str("#![allow(x)]").unwrap();
    let plan = Plan {
        edits: vec![Edit::add("a::f", inner)],
    };
    assert!(matches!(
        apply_plan(&file, &plan),
        Err(PlanError::InvalidAttrTokens { .. })
    ));
}

/// 原子性：计划中任一步失败，整体失败且原 File 不变。
#[test]
fn failure_is_atomic() {
    let file = parse("mod a { fn f() {} }");
    let before = tokens_of(&file);
    let plan = Plan {
        edits: vec![
            Edit::add("a::f", quote!(#[inline])), // 合法
            Edit::remove("a::nope", "allow"),     // 失败
        ],
    };
    assert!(matches!(
        apply_plan(&file, &plan),
        Err(PlanError::NotFound { edit: 1, .. })
    ));
    assert_eq!(tokens_of(&file), before);
}

/// 验收点 4：quote 再解析——新 AST 重新打印成 token 后可再次解析，
/// 且函数体不被重写、item 不重排、内部属性保留。
#[test]
fn quote_reparse_round_trip() {
    let src = concat!(
        "#![allow(dead_code)]\n",
        "mod a {\n",
        "#![doc = \"inner\"]\n",
        "#[allow(unused)]\n",
        "fn f() { let x = 1 +  2; }\n",
        "fn g() {}\n",
        "}\n",
        "mod b { fn f() {} }\n",
    );
    let file = parse(src);
    let plan = Plan {
        edits: vec![
            Edit::add("a::f", quote!(#[inline])),
            Edit::remove("a::f", "allow"),
        ],
    };
    let (new_file, report) = apply_plan(&file, &plan).unwrap();
    assert_eq!(report.len(), 2);

    // quote 再解析：token 流稳定。
    let tokens = quote!(#new_file);
    let reparsed: File = syn::parse2(tokens.clone()).unwrap();
    assert_eq!(quote!(#reparsed).to_string(), tokens.to_string());

    // 文件级内部属性保留。
    assert_eq!(new_file.attrs.len(), 1);
    assert!(new_file.attrs[0].path().is_ident("allow"));
    // 模块内部属性保留。
    let Item::Mod(mod_a) = &new_file.items[0] else {
        panic!("expected mod a")
    };
    assert!(mod_a.attrs.iter().any(|a| a.path().is_ident("doc")));

    // item 不重排。
    assert_eq!(item_names(&new_file), vec!["a", "b"]);
    let a_items: Vec<String> = mod_a
        .content
        .as_ref()
        .unwrap()
        .1
        .iter()
        .filter_map(item_name)
        .collect();
    assert_eq!(a_items, vec!["f", "g"]);

    // 函数体不被重写：f 的 block token 与原来完全一致。
    fn block_of(file: &File) -> String {
        let Item::Mod(m) = &file.items[0] else { panic!() };
        let Item::Fn(f) = &m.content.as_ref().unwrap().1[0] else {
            panic!()
        };
        let block = &f.block;
        quote!(#block).to_string()
    }
    assert_eq!(block_of(&file), block_of(&new_file));

    // a::f 最终只有 inline；a::g 与 b::f 无属性。
    let f_attrs = attrs_at(&new_file.items, &["a", "f"]);
    assert_eq!(f_attrs.len(), 1);
    assert!(f_attrs[0].path().is_ident("inline"));
    assert!(attrs_at(&new_file.items, &["a", "g"]).is_empty());
    assert!(attrs_at(&new_file.items, &["b", "f"]).is_empty());
}

/// 一次新增多个属性，span 对应逐条记录。
#[test]
fn add_multiple_attrs_at_once() {
    let file = parse("mod a { fn f() {} }");
    let plan = Plan {
        edits: vec![Edit::add("a::f", quote!(#[inline] #[cold]))],
    };
    let (new_file, report) = apply_plan(&file, &plan).unwrap();
    assert_eq!(attrs_at(&new_file.items, &["a", "f"]).len(), 2);
    assert_eq!(report[0].spans.len(), 2);
    assert!(report[0].spans.iter().all(|p| p.old.is_none() && p.new.is_some()));
}
