# attr-edit

AST 属性编辑计划（attribute edit plan）：

- 以显式路径（如 `a::f`）选中 item；
- 只增删目标 item 的外层属性，保留内部属性（`#![...]`）与无关 token 顺序，
  不重排 item、不重写函数体；
- 同路径歧义（多个同名 item）、重复目标、属性参数解析失败都会在应用前拒绝；
- `cfg_attr(...)` 括号内的文本不会被当成直接属性增删；
- 任一步失败则整个计划失败，原 `syn::File` 不被修改；
- 成功时返回新 `File` 以及每处编辑的旧/新属性 span 对应表。

## 用法

```rust
use attr_edit::{apply_plan, Edit, Plan};
use quote::quote;

let file = syn::parse_file("mod a { fn f() {} } mod b { fn f() {} }").unwrap();
let plan = Plan {
    edits: vec![
        Edit::add("a::f", quote!(#[inline])), // 只给模块 a 的 f 加 inline
        Edit::remove("a::f", "allow"),        // 删除直接 allow，不碰 cfg_attr 里的
    ],
};
let (new_file, report) = apply_plan(&file, &plan).unwrap();
```

## 局部命令

```text
cargo test -p attr-edit            # 构建并运行全部验收测试
cargo test -p attr-edit --test plan # 只跑集成验收测试
cargo doc -p attr-edit --no-deps   # 查看 API 文档
```
