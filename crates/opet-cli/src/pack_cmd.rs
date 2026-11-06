//! `opet pack …`：内容包的用户面（FLOW §7 那七步的形状）。
//!
//! 三条口径，改这里之前先认：
//!
//! 1. **命令面薄，判断面厚**。本文件只管参数、打印、退出码；"这个包能不能装、装坏了怎么退"
//!    全在 [`petstore::pack`]，那里有 16 条测试逐条钉着（`opet-cli` 没有 dev-dependencies，
//!    把文件系统断言写进来就等于给仓库添一条新依赖）。
//! 2. **拒绝放在最便宜的一侧**。`--max-mb` 调小、`--verify-signature` 这种"本版本没有的开关"
//!    都在**开库、开包之前**判掉 ⇒ 它们既不建目录也不读包（同 `pull --currency` 那条的形状）。
//! 3. **`show` / `verify` 完全只读**：不开数据库。这是 §7 那张表里 `PackArchive` 那一行的
//!    唯一时机（"装之前"的预览与完整性检查），也是这台机器还没装过包时唯一能跑的两条。
//!
//! 退出码沿用 [`gacha_cmd`] 那一套：`Err(ExitCode)` 表示**已经打过原因**，`main` 不再附 usage。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use petcore::assets::{AssetDir, AssetSource, installed_dir};
use petcore::gacha::ItemKind;
use petstore::Store;
use petstore::pack::{self, InstallOpts, MAX_PACK_MB};
use petstore::paths::{AssetRoot, DbPath, Env};

use crate::gacha_cmd::load_catalog;
use crate::{ints, word};

pub const USAGE: &str = "\
P1 可用（内容包；判断逻辑与它的 16 条测试在 `petstore::pack`）：
  opet pack show FILE [--item ID] [--max-mb N]
      只读：打印包内 manifest.toml 的全部字段与每项的成员名。不建库、不写盘
  opet pack verify FILE [--catalog FILE] [--max-mb N]
      只读门禁：逐件预检\"包里的清单能不能解析、sheet 成员在不在、有没有撞随机池\"（§3.11.5）。
      全程不解压 ⇒ 缺哪件说得出名字。末行 GATE … result=PASS/FAIL
      ⚠ 撞池分两种：**付费包**撞池 = 违反 §3.11.5 ⇒ 判红整包拒；**免费包**撞池 = 官方像素包的形状 ⇒
        只提示，装的时候那几件**只落像素、不给所有权**（所有权只能来自抽/兑换）
  opet pack install FILE [--db PATH] [--catalog FILE] [--force] [--redeem CODE] [--max-mb N]
      装包：门禁全过才碰磁盘 ⇒ 被拒绝的包**一个字都不留**（先解进 <根>/.tmp.<id>/，再逐目录 rename）。
      库存写 obtained_from='pack:<id>'（与抽卡那条所有权路径分开记，J4）。
      --force 只在\"目标目录属于别人\"时接管，接管的项逐条打印；自己的目录重装不需要它。
      --max-mb 只许调**大**（默认 64）：调小不是\"关掉体积门禁\"，而是一台机器上永远装不了包
  opet pack list [--db PATH]
      账本侧（content_packs + 库存）与磁盘侧（两个资产根）并排打印，并把两侧的差异点名 ——
      §9.2 第 20 条那句\"两条路互不相认\"就靠这一条命令对上线
  opet pack uninstall PACK_ID [--db PATH]
      只删 <根>/.installed_by/<id> 里列的文件，目录按\"最深先、只删空目录\"剪；
      别人手工放进去的东西一律保留。卸载删的是文件与库存行，**不动** content_packs/entitlements（退款是另一条路）

三条都不写默认存档：写 $XDG_DATA_HOME/opet/opet.db 需要 --db PATH 或 --allow-default-archive
（或 OPET_ALLOW_DEFAULT_ARCHIVE=1），否则报 DefaultArchiveNotAllowed ⇒ 随手一条命令写脏真存档。
⚠ v1 **没有** minisign 验签实现 ⇒ 没有 --verify-signature 这个开关（给了就报错，不静默当没看见）。
未签名的第三方包照装，但会打\"没有 signature\"那一行（§3.11.2：这是渠道特性，不是漏洞）。
";

/// 一条命令的走向：`Ok(())` = 0，`Err(code)` = 已自行打印原因并定好退出码。
type Cmd = Result<(), ExitCode>;

fn fail(reason: &str) -> ExitCode {
    eprintln!("错误: {reason}");
    ExitCode::FAILURE
}

pub fn dispatch(argv: &[String]) -> ExitCode {
    let cmd = argv.first().map(String::as_str).unwrap_or("");
    let rest: Vec<String> = argv.iter().skip(1).cloned().collect();
    match dispatch_inner(cmd, &rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// 第一参数是子命令名（`install`/`list`/…），`other` 分支的报错要提它 ⇒ 与 `gacha_cmd` 同形。
fn dispatch_inner(cmd: &str, args: &[String]) -> Cmd {
    match cmd {
        "show" => pack_show(args),
        "verify" => pack_verify(args),
        "install" => pack_install(args),
        "list" => pack_list(args),
        "uninstall" => pack_uninstall(args),
        other => Err(fail(&format!(
            "未知的 pack 子命令: {other}（可用：show, verify, install, list, uninstall；完整参数 `opet pack --usage`）"
        ))),
    }
}

#[derive(Debug, Default)]
struct Flags {
    /// 位置参数：包文件路径，或 `uninstall` 的包 id。
    target: Option<String>,
    db: Option<PathBuf>,
    catalog: Option<PathBuf>,
    item: Option<String>,
    redeem: Option<String>,
    max_mb: u64,
    force: bool,
    allow_default: bool,
    /// 只用来**点名拒绝**（v1 没有验签实现）。给它任何语义就是假开关。
    verify_signature: bool,
}

impl Flags {
    fn new() -> Self {
        Self {
            max_mb: MAX_PACK_MB,
            ..Default::default()
        }
    }
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => f.db = Some(PathBuf::from(word(args, &mut i, "--db")?)),
            "--catalog" => f.catalog = Some(PathBuf::from(word(args, &mut i, "--catalog")?)),
            "--item" => f.item = Some(word(args, &mut i, "--item")?),
            "--redeem" => f.redeem = Some(word(args, &mut i, "--redeem")?),
            "--max-mb" => f.max_mb = ints(args, &mut i, "--max-mb")?,
            "--force" => f.force = true,
            "--allow-default-archive" => f.allow_default = true,
            "--verify-signature" => f.verify_signature = true,
            other if other.starts_with("--") => return Err(format!("未知选项: {other}")),
            other => {
                if f.target.is_some() {
                    return Err(format!(
                        "多余的位置参数: {other}（每条 pack 子命令只吃一个：包文件路径或包 id）"
                    ));
                }
                f.target = Some(other.to_string());
            }
        }
        i += 1;
    }
    Ok(f)
}

/// 只花字符串比较的那几条拒绝，**必须在开库与开包之前**跑完（口径 2）。
fn check_flags(f: &Flags) -> Result<(), String> {
    if f.verify_signature {
        // 一个"接了但什么都不做"的开关比没有这个开关更坏（`--cell` 那次假开关的直接教训）。
        return Err(
            "--verify-signature 不存在：v1 没有 minisign 验签实现（§3.11.2）。未签名的包照装，\
             `install` 会打\"没有 signature\"那一行；要真验签，得先单独一轮把验签实现与公钥管理做出来。"
                .to_string(),
        );
    }
    if f.max_mb < MAX_PACK_MB {
        return Err(format!(
            "--max-mb {} 小于 §3.11.1 的 {MAX_PACK_MB} MiB 上限 ⇒ 拒绝。这条门禁只许调**大**：\
             调小它不是\"关掉体积门禁\"，而是让这台机器永远装不了包（一个假开关）。",
            f.max_mb
        ));
    }
    Ok(())
}

/// 取位置参数；`usage` 是写给用户看的那条完整用法。
fn target(f: &Flags, usage: &str) -> Result<String, ExitCode> {
    f.target
        .clone()
        .ok_or_else(|| fail(&format!("{usage} 还缺一个参数（见 `opet pack --usage`）")))
}

/// 开库。**惰性**：只有 `install` / `uninstall` / `list` 的账本侧走这里，`show` / `verify` 永不建库。
fn open_store(f: &Flags) -> Result<Store, ExitCode> {
    let db = DbPath::resolve(f.db.as_deref());
    let allowed = f.allow_default || std::env::var_os(petstore::db::ALLOW_DEFAULT_ENV).is_some();
    Store::open_with(&db, allowed).map_err(|e| {
        fail(&format!(
            "开库 {}（{}）失败: {e}",
            db.path.display(),
            db.source.as_str()
        ))
    })
}

fn mb(max_mb: u64) -> String {
    format!("{max_mb} MiB")
}

// ---- show ----

fn pack_show(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    check_flags(&f).map_err(|e| fail(&e))?;
    let file = target(&f, "opet pack show FILE")?;
    let pack = pack::Pack::open(Path::new(&file), f.max_mb).map_err(|e| fail(&e))?;
    let m = &pack.manifest;
    println!(
        "pack show file={} id={} kind={} store={}",
        file, m.id, m.kind, m.store
    );
    // 12 个字段全打：装包写进 `content_packs` 的就是这一份，逐字段可见才谈得上复核。
    println!(
        "  format={} title={:?} price_cents={:?} currency_code={:?} is_dlc={}",
        m.format, m.title, m.price_cents, m.currency_code, m.is_dlc
    );
    println!(
        "  signature={:?} release_notes={:?}",
        m.signature, m.release_notes
    );
    println!(
        "  pack_path={:?}（清单里写的是发布机路径 ⇒ 装机时以调用方给的为准）",
        file
    );
    println!(
        "  体积 bytes={} entries={} 上限={} items={}",
        pack.bytes,
        pack.entry_count,
        mb(f.max_mb),
        pack.items.len()
    );
    for (kind, id) in &pack.items {
        if let Some(want) = &f.item
            && want != id
        {
            continue;
        }
        let files = pack.files_of(*kind, id);
        println!("  {kind}/{id} 成员 {} 个:", files.len());
        for name in files {
            println!("    {name}");
        }
    }
    if m.signature.is_none() {
        println!("  ⚠ 没有 signature ⇒ 未经第三方验证（§3.11.2：未签照装是特性，不是漏洞）");
    }
    Ok(())
}

// ---- verify ----

fn pack_verify(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    check_flags(&f).map_err(|e| fail(&e))?;
    let file = target(&f, "opet pack verify FILE")?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let pack = pack::Pack::open(Path::new(&file), f.max_mb).map_err(|e| fail(&e))?;
    let check = pack.check(&catalog);
    println!(
        "pack verify file={} id={} catalog={} {}",
        file,
        pack.manifest.id,
        catalog.id,
        check.summary()
    );
    for item in &check.items {
        if item.is_ok() {
            println!("  OK {}/{}", item.kind.as_str(), item.id);
        } else {
            for p in &item.problems {
                println!("  ⚠ {}/{}: {p}", item.kind.as_str(), item.id);
            }
        }
    }
    if !check.forbidden.is_empty() {
        println!(
            "  ✗ 付费包撞随机池 ⇒ 违反 §3.11.5（付费包卖池外内容），整包拒绝: {:?}",
            check.forbidden
        );
    } else if !check.in_pool.is_empty() {
        // 免费包带池内件是**官方像素包**的形状：像素要落（否则 INV 门禁永远绿不了），
        // 但所有权一行都不给（否则一条 install 把整池送完）。这一批必须点名。
        println!(
            "  ℹ 这 {} 件在随机池里 ⇒ 装包只落像素、不给所有权（所有权只能来自抽/兑换）: {:?}",
            check.in_pool.len(),
            check.in_pool
        );
    }
    let pass = check.bad_items() == 0 && check.forbidden.is_empty();
    println!(
        "GATE pack_verify bad={} in_pool={} forbidden={} result={}",
        check.bad_items(),
        check.in_pool.len(),
        check.forbidden.len(),
        if pass { "PASS" } else { "FAIL" },
    );
    if pass { Ok(()) } else { Err(ExitCode::FAILURE) }
}

// ---- install ----

fn pack_install(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    check_flags(&f).map_err(|e| fail(&e))?;
    let file = target(&f, "opet pack install FILE")?;
    // 库先开：默认存档门禁没过 ⇒ 连包都不该开（口径 2）。
    let mut store = open_store(&f)?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let pack = pack::Pack::open(Path::new(&file), f.max_mb).map_err(|e| fail(&e))?;
    let env = Env::from_process();
    let opts = InstallOpts {
        force: f.force,
        redeem: f.redeem.clone(),
    };
    let out = pack::install(&pack, &env, &mut store, &catalog, &opts).map_err(|e| fail(&e))?;
    println!(
        "pack install id={} title={:?} files={} bytes={} catalog={}",
        out.pack_id, out.title, out.files, out.bytes, catalog.id
    );
    for r in &out.roots {
        println!("  落地 {r}");
    }
    println!(
        "  库存 +{} 已有 {}（obtained_from='pack:{}'）",
        out.grant.added.len(),
        out.grant.already.len(),
        out.pack_id
    );
    for key in out.grant.added.iter().chain(out.grant.already.iter()) {
        println!("    {key}");
    }
    println!("  权益 {}", out.source);
    if !out.pixels_only.is_empty() {
        // 与 `verify` 那行同一个说法：像素落了盘，所有权一行都没写。
        println!(
            "  ℹ 只落像素、未给所有权 {} 件（在随机池里，只能靠抽/兑换得到）: {:?}",
            out.pixels_only.len(),
            out.pixels_only
        );
    }
    if !out.took_over.is_empty() {
        println!(
            "  ⚠ --force 接管了不属于本包的项: {:?}（原目录已被包里的内容替换）",
            out.took_over
        );
    }
    if out.unsigned {
        println!("  ⚠ 这个包没有 signature ⇒ 第三方未验证（§3.11.2）。装是照装，屏上要标出来。");
    }
    Ok(())
}

// ---- uninstall ----

fn pack_uninstall(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    check_flags(&f).map_err(|e| fail(&e))?;
    let id = target(&f, "opet pack uninstall PACK_ID")?;
    let mut store = open_store(&f)?;
    let env = Env::from_process();
    let out = pack::uninstall(&env, &mut store, &id).map_err(|e| fail(&e))?;
    println!(
        "pack uninstall id={} removed={} pruned_dirs={} inventory_rows={}",
        out.pack_id, out.removed, out.pruned_dirs, out.inventory_rows
    );
    for r in &out.searched {
        println!("  查过 {r}");
    }
    for kept in &out.kept {
        println!("  ⚠ 删不掉，留着并点名: {kept}");
    }
    println!(
        "  content_packs / entitlements 未动：卸载删的是文件与库存行，退款走 `opet license`（§3.4）"
    );
    Ok(())
}

// ---- list ----

fn pack_list(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    check_flags(&f).map_err(|e| fail(&e))?;
    if f.target.is_some() {
        return Err(fail(
            "opet pack list 不吃位置参数（它列的是这台机器上的全部包）",
        ));
    }
    let env = Env::from_process();
    // 两条根：`scan_roots` 的目录列表（磁盘侧）+ 同一批根上的 `AssetDir`（判"某个 id 在不在盘上"）。
    let sources: Vec<(ItemKind, AssetDir)> = ItemKind::ALL
        .into_iter()
        .filter_map(|k| AssetRoot::resolve_in(&env, k).map(|r| (k, installed_dir(&r.path, k))))
        .collect();
    println!("pack list 磁盘侧");
    let mut disk_ids: Vec<String> = Vec::new();
    for listing in pack::scan_roots(&env) {
        println!(
            "  {} items={} problems={}",
            listing.describe,
            listing.items.len(),
            listing.problems.len()
        );
        for it in &listing.items {
            println!("    {it}");
            if let Some((_, id)) = it.split_once('/') {
                disk_ids.push(id.to_string());
            }
        }
        for p in &listing.problems {
            println!("    ⚠ {p}");
        }
    }

    let store = match open_store(&f) {
        Ok(s) => s,
        Err(code) => {
            eprintln!("提示: 账本侧没读到 ⇒ 上面只有磁盘侧，两侧差异未检查。");
            return Err(code);
        }
    };
    let reg = pack::registered(&store).map_err(|e| fail(&e))?;
    let owned = store.owned().map_err(|e| fail(&e.to_string()))?;
    println!("pack list 账本侧 packs={} owned={}", reg.len(), owned.len());
    for r in &reg {
        println!(
            "  {} title={:?} kind={} store={} price={:?} 库存行={} 包文件={}",
            r.id, r.title, r.kind, r.store, r.price_cents, r.inventory_rows, r.pack_path
        );
        if !Path::new(&r.pack_path).is_file() {
            println!("    ⚠ 包文件不在了（已装资产仍在磁盘上，`verify` 要重跑得先把包找回来）");
        }
    }

    // §9.2 第 20 条那句"两条路互不相认"的正面处理：**打印差异而不是合并事实源**。
    // 未拥有的盘上项 = 开发态手工塞的那一类，P2 的换装页会把它们过滤掉（`--allow-unowned-accessories`
    // 是那条的显式开关）；拥有而盘上没有的 = 缺产物，那是内容账，不是接线账。
    println!("pack list 对照");
    let unowned: Vec<&String> = disk_ids
        .iter()
        .filter(|id| !owned.contains(id.as_str()))
        .collect();
    let no_pixels: Vec<&String> = owned
        .iter()
        .filter(|id| !sources.iter().any(|(k, d)| d.locate(*k, id).is_some()))
        .collect();
    for id in &unowned {
        println!("  ⚠ 盘上有而未拥有: {id} ⇒ 换装页不列（J3：不能穿未拥有）");
    }
    for id in &no_pixels {
        println!("  ⚠ 已拥有而盘上没有: {id} ⇒ 穿上会失败（缺的是产物，不是接线）");
    }
    if unowned.is_empty() && no_pixels.is_empty() {
        println!("  两侧一致");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `--max-mb` 只许调大。这条拒绝必须发生在**开库之前** ⇒ 给的路径可以不存在、
    /// 也不许碰 `$XDG_DATA_HOME`（同 `pull --currency` 那条测试的价值所在）。
    #[test]
    fn shrinking_the_size_gate_is_refused_rather_than_accepted() {
        assert_eq!(MAX_PACK_MB, 64, "§3.11.1 的那个 64 MiB");
        for small in ["0", "1", "63"] {
            let args = sv(&["--max-mb", small, "/definitely/absent.petpack"]);
            assert_eq!(
                pack_install(&args),
                Err(ExitCode::FAILURE),
                "--max-mb {small} 不该被当成'这台机器关门禁'的口子"
            );
        }
        // 调大是合法选择（打包的人也许就超了 64），但要过 `ints` 那道非负判定。
        let big = parse_flags(&sv(&["--max-mb", "128"])).unwrap();
        assert_eq!(big.max_mb, 128);
        assert!(check_flags(&big).is_ok());
        assert!(parse_flags(&sv(&["--max-mb", "-1"])).is_err());
        assert!(parse_flags(&sv(&["--max-mb"])).is_err(), "缺值不许当成默认");
    }

    /// 这一版没有验签实现 ⇒ `--verify-signature` 必须**点名拒绝**，而不是"接受并什么都不做"。
    /// 后者就是假开关：用户以为验了。
    #[test]
    fn a_flag_that_would_silently_do_nothing_is_refused_by_name() {
        for sub in ["install", "verify", "show", "list", "uninstall"] {
            let args = sv(&["--verify-signature", "/definitely/absent.petpack"]);
            let got = dispatch_inner(sub, &args);
            assert_eq!(
                got,
                Err(ExitCode::FAILURE),
                "{sub} 必须拒绝 --verify-signature"
            );
        }
        let f = parse_flags(&sv(&["--verify-signature"])).unwrap();
        assert!(f.verify_signature, "解析要记得它，否则拒绝分支永远走不到");
    }

    /// 每条子命令只吃一个位置参数，缺了要报错而不是拿空字符串去拼路径。
    #[test]
    fn each_subcommand_takes_exactly_one_positional() {
        assert_eq!(
            dispatch_inner("install", &[]),
            Err(ExitCode::FAILURE),
            "不给包文件就不该去开库"
        );
        assert!(
            parse_flags(&sv(&["a.petpack", "b.petpack"])).is_err(),
            "第二个位置参数是拼写错误，不是被忽略的那一个"
        );
        // `list` 反过来：它不吃位置参数，给了就该红（否则"拼错的包名"会表现成"这台机器上没装包"）。
        assert_eq!(
            dispatch_inner("list", &sv(&["typo_01"])),
            Err(ExitCode::FAILURE)
        );
    }

    #[test]
    fn an_unknown_subcommand_names_the_five_it_knows() {
        for bad in ["make", "Pack", ""] {
            assert_eq!(dispatch_inner(bad, &[]), Err(ExitCode::FAILURE));
        }
    }

    /// `verify` / `show` 只读 ⇒ 它们**不该**要求 `--allow-default-archive`：在一台还没建过库的
    /// 机器上，验包这件事与"这台机器有没有存档"无关。判据 = 一个不存在的包文件报的是开包的错，
    /// 而不是默认存档门禁的错。
    #[test]
    fn reading_a_pack_never_needs_the_database() {
        for sub in ["verify", "show"] {
            let got = dispatch_inner(sub, &sv(&["/definitely/absent.petpack"]));
            assert_eq!(got, Err(ExitCode::FAILURE), "{sub} 该在开包那一步失败");
        }
        // `--catalog` 指一份不存在的目录也一样：失败来自读目录，不来自数据库。
        let got = dispatch_inner(
            "verify",
            &sv(&[
                "/definitely/absent.petpack",
                "--catalog",
                "/definitely/absent.json",
            ]),
        );
        assert_eq!(got, Err(ExitCode::FAILURE));
    }
}
