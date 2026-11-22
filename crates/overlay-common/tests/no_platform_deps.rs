//! 这一层唯一的立身之本 = **它没有的东西**：图集解码与 polygon 轮廓是各平台客户端共用的，
//! 所以它一旦长出 `wayland-client` / `windows-sys` / `objc2` 中的任何一个，另外两个平台的
//! 客户端就再也依赖不了它 —— 而那**不会编译失败**，只会让下一个平台"顺手再抄一份 atlas.rs"，
//! 于是本项目反复登记的那一族（"两个事实源"：`--cell` 假开关、`anchor.feet` 不进指纹、
//! `equipped` 表零读零写）在共享层正中间再长一次。
//!
//! 所以这条测试读的是**清单本身**而不是任何代码行为：Cargo 会老老实实装上来路不明的依赖，
//! 编译器也不会拒绝 `std::os::unix`，能替这件事把关的只有"在写进 `Cargo.toml` 的那一刻就红"。
//!
//! ## 为什么不是 `grep`
//!
//! ⚠️ **本 crate 自己的 `Cargo.toml` 第 10–11 行的注释里就写着 `wayland-*` 和 `windows-sys`**
//! （那是这条门禁要解释的内容，不是违规）。拿"文件里有没有这个词"当判据会**当场假红**，
//! 而且是本项目已经栽过两次的那一种：字体探针拿 `l10n.rs` 的**含注释原文**做包含测试，
//! 于是把只出现在注释里的 `白` 数成了要上屏的新字（§6.1.15 那条"净 +12 vs 实测净 −1"）。
//! ⇒ 所以下面**先剥注释、只看 `[dependencies]` 那几张表里的键名**，并且把这个剥离本身
//!    当成被测对象（`commented_out_violation_is_not_a_violation` +
//!    `the_real_manifest_proves_stripping_is_load_bearing`）—— 否则"剥了注释"这件事
//!    又是一个从没被非默认值用过的假开关。

use std::path::Path;

/// 允许出现在 `overlay-common` 依赖表里的**全部** crate 名。多一个都红，少一个也红
/// （见 `the_census_is_not_empty`：那条断的就是"这张表真的被读到了"）。
///
/// 白名单而不是黑名单：黑名单只能拦住想到的名字，而"某个 crate 传递性地拖进 wayland 运行时"
/// 这件事在**顶层清单里根本看不见**（`cargo tree -e normal` 才看得见，那属于收工手工核，
/// 写进下面 `transitive_note`）。
const ALLOWED: &[&str] = &[
    "petcore", // 行为内核与清单类型；这一层反过来永不依赖任何 overlay 客户端
    "serde",   // 清单与 polygon 的解析
    "serde_json",
    "image-webp", // `sheet.webp` 解码，纯 Rust（D12 判据①）
    "tempfile",   // 只进 dev-dependencies：测例写临时产物，绝不碰 `~/.local/share/opet/`
];

/// 明确点名的禁止项。**空档由 `ALLOWED` 兜**：这张名单只是让红的时候能说清"你引的是哪家绑定"。
const FORBIDDEN: &[&str] = &[
    // Wayland / X11
    "wayland-client",
    "wayland-protocols",
    "wayland-backend",
    "wayland-bindings",
    "wayland-sys",
    "wayland-scanner",
    "smithay-client-toolkit",
    "calloop",
    "x11",
    "x11rb",
    "xcb",
    "ashpd",
    // Windows
    "windows",
    "windows-sys",
    "windows-core",
    "windows-result",
    "windows-targets",
    "winapi",
    // macOS / iOS
    "objc2",
    "objc",
    "cocoa",
    "core-foundation",
    "core-foundation-sys",
    "core-graphics",
    "metal",
    "metal-sys",
    // 跨平台的"整个渲染层"：它们各自都会把一个平台的运行时拖进来
    "winit",
    "wgpu",
    "glutin",
    "gl",
    // 直接摸 libc ⇒ 平台条件编译的第一入口
    "libc",
];

/// `[dependencies]` / `[dev-dependencies]` / `[build-dependencies]` 三种表头的**末段**。
/// 末段匹配而不是整串匹配 ⇒ `[target.'cfg(windows)'.dependencies]` 那一张也在网里，
/// 那正是"只在某个平台引绑定"最会藏身的地方。
const DEPENDENCY_TABLE_LEAVES: &[&str] =
    &["dependencies", "dev-dependencies", "build-dependencies"];

/// 一张 TOML 表头是不是"依赖的地方"，以及它是哪一种。
///
/// ⚠️ `[dependencies.foo]` 那种**逐项表**不是依赖集合：它下面的键是 `version` / `features`，
/// 照"含 dependencies"收就会把 `version` 当成一个 crate 报出来（红得莫名其妙，且白名单永远
/// 补不完）。所以逐项表只把**表头自己点名的那个名字**记进来，表体一行都不读。
///
/// 判据因此是**倒数第二段**而不是第一段：`[dependencies.foo]` 与
/// `[target.'cfg(windows)'.dependencies.windows-sys]` 是同一族（依赖名挂在末段），
/// 而 `[dependencies]` 与 `[target.'cfg(windows)'.dependencies]` 是同一族（末段是表类型）。
/// 第一版这里写错过一次：先拿 `last` 比对 leaves，于是逐项表那一支**根本不可达**
/// （它的 `last` 恰好是 crate 名，永远不等 leaves）⇒ 见
/// `a_violation_is_caught_whatever_way_it_is_written` 的第三例，它就是专门来钉这一支的。
fn classify(header: &str) -> TableKind {
    let segments: Vec<&str> = header
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split('.')
        .map(|s| s.trim().trim_matches(['\'', '"']))
        .collect();
    // `[[bin]]` 那种数组表：末段仍是 `bin`，落在 Irrelevant 里，但明写一下免得被读成漏网。
    if segments
        .last()
        .copied()
        .is_some_and(|leaf| DEPENDENCY_TABLE_LEAVES.contains(&leaf))
    {
        TableKind::Collection
    } else if segments.len() >= 2 && DEPENDENCY_TABLE_LEAVES.contains(&segments[segments.len() - 2])
    {
        // 逐项表 ⇒ 依赖名 = 末段；表体不再读。
        TableKind::Item(segments[segments.len() - 1].to_string())
    } else {
        // 其余表头（[package] / [[bin]] / [features] / [profile.*]）一律忽略。
        TableKind::Irrelevant
    }
}

enum TableKind {
    /// `[dependencies]` 那类"键名 = crate 名"的集合表
    Collection,
    /// `[dependencies.foo]` 那类逐项表：只认表头点名的 `foo`
    Item(String),
    /// 与依赖无关的表
    Irrelevant,
}

/// 剥掉 **TOML** 一行里的注释（`#`）。
///
/// ⚠️ 这个函数**只管 TOML**。Rust 侧另有一条（`rust_code_only`）：第一版我把这一个函数
/// 同时用在两种文件上，于是它在本 crate 自己的 `src/lib.rs` 第 17 行那句文档注释上假红
/// —— 那一行写的正是"这里不许出现 `std::os::unix`"这条规矩本身。"剥注释"在两处都有，
/// 但**符号不同**（`#` vs `//`）⇒ 两条规则两个函数，名字里把语言写出来。
///
/// 边界（明写，不是遗漏）：值里的 `#`（`foo = "a#b"`）会被当注释起点。下面只在 `=` **之前**
/// 取键名，键名不可能在那个位置之后 ⇒ 对本测试要读的东西没有影响。
fn strip_toml_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// 本 crate 的源码里"平台条件出口"要抓的那几个词，以及它们的后果。
const PLATFORM_EXITS: &[&str] = &["std::os::unix", "std::os::windows", "std::os::macos"];

/// Rust 一行的"代码部分" = `//` 之前的那段（`//!` 与 `///` 同样是 `//` 开头 ⇒ 一并剥掉）。
///
/// 两条已知边界，都登记在这里而不是藏进实现：
/// 1. **块注释 `/* … */` 不处理**。本 crate 的 src/ 里现在一处没有 ⇒ 单行扫描够用；
///    真要出现，它跨行，单行扫描给不出正确结果，那需要的是词法器而不是这条测试的价格。
///    ⚠️ **但"一处没有"这件事不能用 `grep -c '/\*'` 去量**：现量它给 1，而那 1 处是
///    `src/lib.rs:12` 文档注释里的路径 glob `assets/pets/*_192`（`/` 后紧跟 `*`）⇒ 那是
///    读错通道的尺子，与 §6.1.20⑮ 那条"配件门禁量的是 R 不是 alpha"同一族。要量就得先过
///    `rust_code_only` 再找 `/*`。
/// 2. 字符串里含 `//` 的（URL）会把行切早 ⇒ 方向是**漏报**不是误报。这里找的是
///    `std::os::*`，而本 crate 的 src/ 里连一个含 `//` 的字符串字面量都没有
///    （`grep -n '"[^"]*//'` 空 ⇒ 这条边界现在是**未触发**而不是**不存在**）。
///    要的是"健康状态不许红"，这两条边界都不伤那个目标。
fn rust_code_only(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// 纯函数：给一段 Rust 源码，返回它代码部分里出现的平台出口（带行号）。
/// 之所以从文件扫描里拆出来：`sources_have_no_platform_exit` 只能对真文件断言"没命中"，
/// 而"没命中"读不出扫描器是好的还是根本没工作 ⇒ 必须能拿合成件把它**判红**（同
/// `a_violation_is_caught_whatever_way_it_is_written` 那条理由）。
fn platform_exits_in_rust(text: &str) -> Vec<(usize, &'static str)> {
    let mut out = Vec::new();
    for (no, line) in text.lines().enumerate() {
        let code = rust_code_only(line);
        for needle in PLATFORM_EXITS {
            if code.contains(needle) {
                out.push((no + 1, *needle));
            }
        }
    }
    out
}

/// 从一行里取"这一行声明的那个依赖名"，取不到就返回 `None`。
fn dep_key_in(line: &str) -> Option<String> {
    let before_eq = strip_toml_comment(line).split('=').next()?.trim();
    // 内联表 `foo = { ... }` 与 `foo = "1"` 两种写法都以键名开头；
    // `[package]` 那种由 classify 拦在外面，这里再兜一层"长得像表头/数组续行就跳过"。
    if before_eq.is_empty()
        || before_eq.starts_with('[')
        || before_eq.starts_with(']')
        || before_eq.contains('.')
        || before_eq.contains('[')
    {
        return None;
    }
    Some(before_eq.trim_matches(['\'', '"']).to_string())
}

/// 把一份 `Cargo.toml` 文本读成它声明的依赖名集合（顺序 = 文件里出现的顺序，不去重 ⇒ 重复
/// 声明会被看见两次，那是 bug 不是噪音）。
fn declared_dependencies(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut table = TableKind::Irrelevant;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            table = classify(line);
            if let TableKind::Item(name) = &table {
                out.push(name.clone());
            }
            continue;
        }
        if matches!(table, TableKind::Collection)
            && let Some(name) = dep_key_in(line)
        {
            out.push(name);
        }
    }
    out
}

fn manifest_text() -> String {
    // `include_str!` 而不是运行时 `fs::read`：清单变了必须重编这条测试，否则"改了 Cargo.toml
    // 而门禁还在绿"正是本项目登记过的那类"计数器不读新东西"。
    include_str!("../Cargo.toml").to_string()
}

/// 断言用的排序副本（`Vec<&str>` 逐项比较对顺序敏感，而依赖写在哪个表里、什么顺序都不该影响结论）。
fn sorted(names: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = names.iter().map(String::as_str).collect();
    v.sort_unstable();
    v
}

// ── ① 真清单 ────────────────────────────────────────────────────────────────

/// 本 crate 声明的每一个依赖都必须在白名单里。
#[test]
fn every_declared_dependency_is_on_the_allowlist() {
    let deps = declared_dependencies(&manifest_text());
    let bad: Vec<&String> = deps
        .iter()
        .filter(|d| !ALLOWED.contains(&d.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "overlay-common 的 Cargo.toml 里出现了白名单外的依赖：{bad:?}\n\
             ⇒ 这一层是**三个平台的客户端共用**的那一层（§3.7 / D12），\
             任何一家显示服务器的绑定都会让另外两家依赖不了它，\n\
             于是下一步就是「再抄一份 atlas.rs」= 两个事实源。\n\
             允许的全集 = {ALLOWED:?}\n\
             \n\
             如果这次加的确实是无平台属性的纯 Rust 库：把它补进本文件的 ALLOWED，\n\
             并按 D8 逐条读它的 LICENSE 原文（含 `ls` 它目录确认没有 EXCEPTION/附加条款），\n\
             再把论证写进根 Cargo.toml 的注释块 —— 那三件事是这个例外唯一的价格。",
    );
}

/// 与①配对的禁止项名单。①已经能拦住它，这条存在的唯一理由是**红的时候那句话要说得出名字**：
/// "白名单外"和"你引的是 wayland"是两种完全不同的通知。
#[test]
fn no_named_platform_binding_is_declared() {
    let deps = declared_dependencies(&manifest_text());
    let hit: Vec<&String> = deps
        .iter()
        .filter(|d| FORBIDDEN.contains(&d.as_str()))
        .collect();
    assert!(
        hit.is_empty(),
        "overlay-common 直接声明了平台绑定：{hit:?} ⇒ 见本文件模块头那句「立身之本是它没有的东西」"
    );
}

/// ⚠️ **反面对照之一：证明上面两条不是在读一张空表**。
/// 没有这条，`declared_dependencies` 一旦解析不出来（表头写法变了、剥剥剥错了）
/// ①②会双双"绿"，而它们验的是"零个依赖"。这是本项目"计数器为 0 不等于代码对"那一族的
/// 直接对应物（`延后 0` / `看门狗 0` / `SKIP + test result: ok` 三条都栽在这里）。
#[test]
fn the_census_is_not_empty() {
    let deps = declared_dependencies(&manifest_text());
    assert!(
        deps.len() >= 4,
        "从 Cargo.toml 只数出 {} 个依赖（{deps:?}）⇒ 解析器或表头写法漂了，\
         那么 no_platform_deps 的两条正向断言正在验一张空表",
        deps.len()
    );
    let mut want: Vec<&str> = ALLOWED.to_vec();
    want.sort_unstable();
    assert_eq!(
        sorted(&deps),
        want,
        "本 crate 的依赖全集与 ALLOWED 不再逐字相同 ⇒ 要么真加了依赖（上一条会先红，\
         除非加的名字恰好重复），要么 ALLOWED 里留着一个已经不用的名字。\
         后者也要红：白名单里躺着一条用不上的项 = 它随时能被偷偷用回去。"
    );
}

// ── ② 判据自己得能被红 ──────────────────────────────────────────────────────

/// 反面对照之二：坏清单必须被**这套函数**判出来，而不是被"刚好在清单里搜字符串"判出来。
/// 逐例点名了三种藏身写法，它们分别对应解析器的三处逻辑。
#[test]
fn a_violation_is_caught_whatever_way_it_is_written() {
    let cases: [(&str, &str); 4] = [
        (
            "[dependencies]\npetcore = { workspace = true }\nwayland-client = \"0.31.15\"\n",
            "wayland-client",
        ),
        (
            "[package]\nname = \"x\"\n[dependencies]\nserde = { workspace = true }\n\
             [target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.61\"\n",
            "windows-sys",
        ),
        (
            "[dependencies.smithay-client-toolkit]\nversion = \"0.19\"\nfeatures = [\"calloop\"]\n",
            "smithay-client-toolkit",
        ),
        (
            "[dependencies]\ncalloop = \"0.13\"\n[dev-dependencies]\ntempfile = \"3\"\n",
            "calloop",
        ),
    ];
    for (toml, expected) in cases {
        let found = declared_dependencies(toml);
        assert!(
            found.iter().any(|d| d == expected),
            "这份坏清单没被读出来：应含 {expected}，实读 {found:?}\n原文:\n{toml}"
        );
        // 而且不许把逐项表的表体（`version` / `features`）当成 crate 名。
        assert!(
            !found.iter().any(|d| d == "version" || d == "features"),
            "把逐项表 `[dependencies.x]` 的表体当成依赖了：{found:?}"
        );
    }
}

/// 反面对照之三，**方向相反**：注释里的违禁词不算违规。
/// 这条与上一条一起才把"剥注释"这个动作钉成被测对象 —— 少任何一条，剥得多/剥得少都无人知晓。
#[test]
fn commented_out_violation_is_not_a_violation() {
    let toml = "[dependencies]\n# wayland-client = \"0.31\"\npetcore = { workspace = true } # objc2 只是注释\n";
    let found = declared_dependencies(toml);
    assert_eq!(
        sorted(&found),
        vec!["petcore"],
        "注释里的依赖被当成声明了（实读 {found:?}）⇒ 门禁会假红，\
         而本项目对假红的处置从来不是「放宽阈值」而是「修仪器」"
    );
}

/// ⚠️ **这条用的是真清单，不是合成件**，因为它是本模块头那句警告的证据：
/// 本 crate 的 `Cargo.toml` **注释里就明写着** `wayland-*` 与 `windows-sys`。
/// ⇒ 拿 `grep` 当门禁会**在健康状态下红**，而"先把红着的门禁放宽到能过"是这一族事故的标准成因。
#[test]
fn the_real_manifest_proves_stripping_is_load_bearing() {
    let text = manifest_text();
    assert!(
        text.contains("wayland") && text.contains("windows-sys"),
        "前提变了：Cargo.toml 的注释里不再出现平台名字 ⇒ 本条测试失去意义，\
         请把它删掉而不是留着当摆设（它存在的理由就是「这个词在文本里但不在依赖里」）"
    );
    let deps = declared_dependencies(&text);
    for name in FORBIDDEN {
        assert!(
            !deps.iter().any(|d| d == name),
            "{name} 现在**真的**是声明出来的依赖了（不是注释）⇒ 该红的是 ①②，不是这条"
        );
    }
}

// ── ③ 依赖之外的第二个入口 ──────────────────────────────────────────────────

/// 白名单拦得住 `Cargo.toml`，拦不住**标准库里的平台门**：`std::os::unix` 不需要任何依赖，
/// 一行就能把 Netlink、`AF_UNIX`、`/proc` 请进来，而 `/proc` 恰好是本项目管理内存读数的方式
/// （`app.rs:5144 proc_self_summary()`）⇒ 让它进共享层 = Windows 侧要么链不上，要么再抄一份。
/// 所以本模块头那句"也不许出现 `std::os::unix`"必须有人执行，不然它是注释里的愿望。
#[test]
fn sources_have_no_platform_exit() {
    // 运行时读目录而不是 include_str! 每个文件：新增 src/*.rs 必然要改 lib.rs 的 `mod` 声明
    // ⇒ 会触发重编 ⇒ 下一次跑就把新文件读进来了。（一处不完美：若加的文件**没**被 lib.rs
    // 声明，它自己不会触发重编，要等下一次别的改动才被扫到 —— 那种文件本来也不参与构建，
    // 而把它做成编译期常量要写死文件名列表，那是"第二份事实源"的开法。）
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for entry in
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("读 {} 失败: {e}", dir.display()))
    {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        files.push(path.file_name().unwrap().to_string_lossy().into_owned());
        hits.extend(
            platform_exits_in_rust(&text)
                .into_iter()
                .map(|(no, needle)| format!("{}:{no}: {needle}", path.display())),
        );
    }
    assert!(
        !files.is_empty(),
        "src/ 里一个 .rs 都没读到 ⇒ 这条在验空集（同 ①② 那条空表风险）"
    );
    assert!(
        hits.is_empty(),
        "overlay-common 出现了平台条件出口：\n  {}\n\
         ⇒ 这一层要能被三家客户端同时依赖。要读 /proc、AF_UNIX、句柄之类，\n\
         把它留在**各自平台的客户端**里（Wayland 侧的 proc_self_summary 就在那儿），别上移。\n\
         \n\
         如果命中的那一行其实是**在解释这条规矩**（文档注释里点名 `std::os::unix`），\n\
         那说明 `rust_code_only` 没剥干净 ⇒ 修仪器，不要放宽这里，也不要删掉那句解释。",
        hits.join("\n  ")
    );
}

/// 反面对照之四，方向与③相反：**真代码里的**平台出口必须被读出来，同时**解释这条规矩的
/// 那句文档注释不许**被读出来。
///
/// 这一条不是加固而是**补锅**：③的第一版把 TOML 的 `#` 剥注释函数直接用在 `.rs` 上，
/// 于是它在 `src/lib.rs` 第 17 行（"这里不许出现 `std::os::unix`"那句说明本身）假红。
/// 只有正向断言"没命中"永远读不出那一类错 —— 同一条理由见
/// `a_violation_is_caught_whatever_way_it_is_written`。
#[test]
fn the_rust_scanner_reads_code_and_not_prose() {
    // 该红的：两处真代码。
    let bad =
        "// 说明\nuse std::os::unix::net::UnixStream;\nlet p = std::os::unix::fs::symlink(a, b);\n";
    let hit = platform_exits_in_rust(bad);
    assert_eq!(
        hit.len(),
        2,
        "真代码里的两处平台出口只读出 {} 处：{hit:?}",
        hit.len()
    );
    assert_eq!(hit[0].0, 2, "行号从 1 起算，命中应在第 2 行");

    // 不该红的：同样几个词只出现在注释里（**这正是本 crate 的真实形状**）。
    let prose = "//! 没有 `std::os::unix`，也没有 `std::os::windows`。\n/// \n\
                 // std::os::macos 也算\nfn ok() {}\n";
    assert!(
        platform_exits_in_rust(prose).is_empty(),
        "注释里的说明被当成代码了 ⇒ 健康状态会红，而这一族的处置是修仪器"
    );

    // 而且本 crate 的真 `src/lib.rs` 现在**必须**是 prose 那一形：它明写着这几个词，
    // 但都在文档注释里 ⇒ ③ 绿而 naive 的 grep 红。这条断言就是那两者之差。
    let lib = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("读 src/lib.rs");
    assert!(
        lib.contains("std::os::unix"),
        "前提变了：src/lib.rs 不再在文档里点名这条规矩 ⇒ 本条失去意义，删掉它别留摆设"
    );
    assert!(
        platform_exits_in_rust(&lib).is_empty(),
        "src/lib.rs 里那个词现在**在代码里**了（不再是说明）⇒ 该红的是 ③"
    );
}
