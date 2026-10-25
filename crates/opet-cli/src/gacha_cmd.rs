//! M3 的命令行面：`gacha-sim` / `catalog` / `pull` / `redeem` / `audit`。
//!
//! 与 `overlay` 一样**自己返回退出码**，不走 `main.rs` 的 `Result<(), String>` 通道 ——
//! 因为其中几条命令的"失败"是**门禁判红**（`gacha-sim` 偏差超限、`pull --currency real_money`
//! 被红线挡下），那是要给 CI `grep` 的干净输出，不该附带一坨 usage。
//!
//! 参数解析继续 hand-rolled `--k v`（与 `sim`/`overlay` 同一套），**不引 clap**：
//! 本项目的口径是"少一个依赖就少一个许可证要核的地方"（D8 的 allowlist）。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use petcore::gacha::sim::{
    BASE_SIGMA, BaseCheck, CHI2_CRITICAL_MILLIONTHS, EFFECTIVE_HORIZON, Fit, MAX_DEV_MILLIONTHS,
    Observed, effective_rates, simulate,
};
use petcore::gacha::{
    Catalog, ChainReport, DEFAULT_RATES_JSON, GachaParams, Rarity, Rates, RatesFile, disclose, hex,
    verify_chain, verify_chain_anchored, verify_replay,
};
use petstore::ledger::PullRow;
use petstore::{DbPath, Store, StoreError};

use crate::{ints, word};

pub const USAGE: &str = "\
M3 可用（headless，不碰显示服务器）：
  opet gacha-sim [--pulls N] [--seed S] [--seeds K] [--catalog FILE] [--rates-file FILE] [--basis N]
      概率门禁：实测分布 vs 公示综合概率（马氏链精确期望，不掷骰）。
      末行 GATE … result=PASS/FAIL，判红即非零退出。
      0.15% 这条门禁按 100 万抽定标（n=10⁶ 时四档的裕量按标准差计 = N 3.0 / R 4.9 / SR 4.6 /
      SSR 8.3 个 SD ⇒ N 最先撞线）：把 --pulls 调小属于冒烟，判红可能只是抽样噪声，不要当回归。
      --seeds K>1 时改为跑 K 个独立 seed 的散布诊断（明确标注非门禁，退出码恒 0）
  opet catalog rates [--out crates/petcore/assets/gacha/rates.json] [--basis N] [--catalog FILE]
      从引擎生成概率公示（含逐件概率）。不写 --out 就打 stdout。
      公示是推导产物，不许手改：改了参数没重跑，gacha-sim 与单测两头都会红
  opet catalog validate [--catalog FILE] [--strict-assets]
      结构校验目录；--strict-assets 要求每个 manifest_path 指向的文件真的存在
  opet pull --count N [--banner ID] [--db PATH] [--dry-run] [--currency NAME] [--seed S] [--allow-reseed]
      抽取并落账本。--dry-run 走内存库：不建库、不写盘、不扣币。
      --currency 一律拒绝（红线 1：随机数不许被真钱驱动），拒绝即 SCHEMA_ERROR。
      --seed 会清空账本与库存（换 seed 后旧链必然验不过），所以必须配 --allow-reseed
  opet redeem --item ID [--db PATH]
      碎片商店自选 SSR（价 = 公示 ssr_redeem_cost）。不掷骰 ⇒ 不进抽取链
  opet audit verify [--db PATH | --file FILE --seed S] [--expect-head HEX] [--banner ID] [--account NAME]
      链完整性 + 重放。带 --file 时**完全不吃数据库**：第三方只需导出文件 + seed + 公示参数
      --expect-head HEX 给链尾锚点（`opet pull` 打印的 head=）。**不给就查不出截尾**：
      哈希链只让每条记住上一条，删掉末尾几行剩下的部分照样自洽 ⇒ 只验'没被改'，不验'没被删'
  opet audit export --out FILE [--db PATH] [--banner ID] [--account NAME]
      导出 JSONL 账本

写默认存档（$XDG_DATA_HOME/opet/opet.db）需要 --allow-default-archive 或
OPET_ALLOW_DEFAULT_ARCHIVE=1，否则报 DefaultArchiveNotAllowed ⇒ 随手一条命令写脏真存档。
OPET_DB 环境变量与 --db 等价，--db 优先。
";

/// 一条命令的走向：`Ok(())` = 0，`Err(code)` = 已自行打印原因并定好退出码。
type Cmd = Result<(), ExitCode>;

fn fail(reason: &str) -> ExitCode {
    eprintln!("错误: {reason}");
    ExitCode::FAILURE
}

/// 红线 4 的 CLI 落点：`--currency` 这个**参数本身**就是非法的。
///
/// 故意**不去解析**这个值：一旦 `Currency::parse("real_money")` 之类的东西存在，
/// 它就迟早被别处调用，"真钱驱动随机数"于是有了合法路径。这里只做字符串拒绝。
/// 输出逐字含 `SCHEMA_ERROR`（§10.2:960），CI 可以 grep 而不是靠退出码猜。
fn refuse_currency(raw: &str) -> ExitCode {
    eprintln!(
        "SCHEMA_ERROR: --currency={raw} 被拒绝。抽取的驱动货币由 `gacha_banner.currency` \
         的 CHECK 固定为 companion_time/shards，真钱不能驱动随机数（硬约束 1）。 \
         要买内容请走明码标价的确定性内容包（`content_packs.price_cents`）。"
    );
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

/// `dispatch` 的第一参数是命令名（`gacha-sim`/`catalog`/…），`other` 分支的报错要提它。
fn dispatch_inner(cmd: &str, args: &[String]) -> Cmd {
    match cmd {
        "gacha-sim" => gacha_sim(args),
        "catalog" => catalog(args),
        "pull" => pull(args),
        "redeem" => redeem(args),
        "audit" => audit(args),
        other => Err(fail(&format!(
            "未知的 {cmd} 参数: {other}（可用：gacha-sim, catalog, pull, redeem, audit）"
        ))),
    }
}

struct Flags {
    pulls: u64,
    seed: u64,
    seed_explicit: bool,
    seeds: u64,
    count: u32,
    count_explicit: bool,
    banner: String,
    account: String,
    db: Option<PathBuf>,
    out: Option<PathBuf>,
    file: Option<PathBuf>,
    catalog: Option<PathBuf>,
    rates_file: Option<PathBuf>,
    item: Option<String>,
    currency: Option<String>,
    basis: u64,
    expect_head: Option<String>,
    dry_run: bool,
    strict_assets: bool,
    allow_default: bool,
    allow_reseed: bool,
    verbose: bool,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            pulls: EFFECTIVE_HORIZON,
            seed: 1,
            seed_explicit: false,
            seeds: 0,
            count: 1,
            count_explicit: false,
            banner: petstore::DEFAULT_BANNER.to_string(),
            account: petstore::DEFAULT_PROFILE.to_string(),
            db: None,
            out: None,
            file: None,
            catalog: None,
            rates_file: None,
            item: None,
            currency: None,
            basis: EFFECTIVE_HORIZON,
            expect_head: None,
            dry_run: false,
            strict_assets: false,
            allow_default: false,
            allow_reseed: false,
            verbose: false,
        }
    }
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pulls" => f.pulls = ints(args, &mut i, "--pulls")?,
            "--basis" => f.basis = ints(args, &mut i, "--basis")?,
            "--seed" => {
                f.seed = ints(args, &mut i, "--seed")?;
                f.seed_explicit = true;
            }
            "--seeds" => f.seeds = ints(args, &mut i, "--seeds")?,
            "--count" => {
                let n = ints(args, &mut i, "--count")?;
                f.count = u32::try_from(n).map_err(|_| format!("--count 最大 u32，实得 {n}"))?;
                f.count_explicit = true;
            }
            "--banner" => f.banner = word(args, &mut i, "--banner")?,
            "--account" => f.account = word(args, &mut i, "--account")?,
            "--db" => f.db = Some(PathBuf::from(word(args, &mut i, "--db")?)),
            "--out" => f.out = Some(PathBuf::from(word(args, &mut i, "--out")?)),
            "--file" => f.file = Some(PathBuf::from(word(args, &mut i, "--file")?)),
            "--catalog" => f.catalog = Some(PathBuf::from(word(args, &mut i, "--catalog")?)),
            "--rates-file" => {
                f.rates_file = Some(PathBuf::from(word(args, &mut i, "--rates-file")?))
            }
            "--item" => f.item = Some(word(args, &mut i, "--item")?),
            "--expect-head" => f.expect_head = Some(word(args, &mut i, "--expect-head")?),
            "--currency" => f.currency = Some(word(args, &mut i, "--currency")?),
            "--dry-run" => f.dry_run = true,
            "--strict-assets" => f.strict_assets = true,
            "--allow-default-archive" => f.allow_default = true,
            "--allow-reseed" => f.allow_reseed = true,
            "--verbose" => f.verbose = true,
            other => return Err(format!("未知选项: {other}")),
        }
        i += 1;
    }
    Ok(f)
}

fn load_catalog(explicit: Option<&Path>) -> Result<Catalog, String> {
    match explicit {
        None => Catalog::default_v1().map_err(|e| e.to_string()),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("读目录 {} 失败: {e}", p.display()))?;
            let c = Catalog::from_json(&text).map_err(|e| e.to_string())?;
            c.validate().map_err(|e| e.to_string())?;
            Ok(c)
        }
    }
}

// ---- gacha-sim ----

fn fmt_pair(key: &str, v: u64) -> String {
    format!("{key}{v}")
}

/// 门禁行的唯一打印处。`tag` 让单 seed 与散布诊断共用同一套键名（CI 只 grep 一种写法）。
fn print_fit(tag: &str, obs: &Observed, fit: &Fit, elapsed_s: f64) {
    println!(
        "{tag} pulls={} counts={}{}{}{}",
        obs.pulls,
        obs.counts[Rarity::N.index()],
        fmt_pair(" R=", obs.counts[Rarity::R.index()]),
        fmt_pair(" SR=", obs.counts[Rarity::SR.index()]),
        fmt_pair(" SSR=", obs.counts[Rarity::SSR.index()]),
    );
    println!(
        "{tag} published={}{}{}{}",
        fit.published[0],
        fmt_pair(" R=", fit.published[1] as u64),
        fmt_pair(" SR=", fit.published[2] as u64),
        fmt_pair(" SSR=", fit.published[3] as u64),
    );
    println!(
        "{tag} dev_N={} dev_R={} dev_SR={} dev_SSR={} unit=millionths gate={MAX_DEV_MILLIONTHS} worst={}",
        fit.dev_millionths[0],
        fit.dev_millionths[1],
        fit.dev_millionths[2],
        fit.dev_millionths[3],
        fit.worst_dev(),
    );
    println!(
        "{tag} chi2={:.6} chi2_millionths={} gate_millionths={CHI2_CRITICAL_MILLIONTHS} p={:.6} (p 仅诊断)",
        fit.chi2(),
        fit.chi2_millionths,
        fit.p_value(),
    );
    if elapsed_s > 0.0 {
        println!(
            "{tag} throughput={:.0} pulls/s wall={elapsed_s:.3}s",
            obs.pulls as f64 / elapsed_s.max(1e-9)
        );
    }
}

fn gacha_sim(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let params = GachaParams::V1;
    // 参照物 = 公示的那一列。先读**仓库里那份文件**：要验的是"公示说的话"，
    // 不是"参数此刻算出的话"。文件与参数不一致时这里就红，不用等 disclose 的单测。
    let published =
        published_column(&params, f.basis, f.rates_file.as_deref()).map_err(|e| fail(&e))?;

    if f.seeds > 1 {
        return spread(&params, &catalog, &published, &f);
    }
    if f.pulls == 0 {
        return Err(fail("--pulls 至少 1"));
    }

    let started = std::time::Instant::now();
    let obs = simulate(&params, &catalog, f.seed, f.pulls).map_err(|e| fail(&e.to_string()))?;
    let wall = started.elapsed().as_secs_f64();
    let fit = Fit::new(obs.counts, obs.pulls, &published);
    let base = BaseCheck::run(&obs.base_counts, obs.base_pulls, &params.rates);

    println!(
        "gacha-sim seed={} catalog={} banner={} basis={} max_dev_millionths={MAX_DEV_MILLIONTHS}",
        f.seed, catalog.id, catalog.banner_id, f.basis
    );
    print_fit("gacha-sim", &obs, &fit, wall);
    println!(
        "gacha-sim base_subset={} base_N={} base_R={} base_SR={} base_SSR={} sigma={BASE_SIGMA} ok={}",
        obs.base_pulls,
        obs.base_counts[0],
        obs.base_counts[1],
        obs.base_counts[2],
        obs.base_counts[3],
        base.ok(),
    );
    println!(
        "gacha-sim floor_hits={} hard_pity_hits={} max_pity={} duplicates={} shards={}",
        obs.floor_hits, obs.hard_pity_hits, obs.max_since_ssr, obs.duplicates, obs.shards
    );
    let pass = fit.dev_ok() && fit.chi2_ok() && base.ok() && obs.total() == obs.pulls;
    println!(
        "GATE dev_ok={} chi2_ok={} base_ok={} total_ok={} result={}",
        fit.dev_ok(),
        fit.chi2_ok(),
        base.ok(),
        obs.total() == obs.pulls,
        if pass { "PASS" } else { "FAIL" },
    );
    if pass { Ok(()) } else { Err(ExitCode::FAILURE) }
}

/// 散布诊断：K 个独立 seed，每个 `--pulls` 抽。**明确不是门禁**（它只回答两件事：
/// "<0.15% 不是这一个 seed 走运"，以及"逐 seed 的散布宽度是不是纯统计噪声"），退出码恒 0。
///
/// 单 seed 在 10⁵ 抽上超 0.15% 是**预期的**（N 档 SD 就有 1550 百万分位），所以判读看
/// `pooled` 那一行（K×N 抽合并后的同一条门禁），`seeds_over_gate` 只提供"噪声量级"的印象。
fn spread(params: &GachaParams, catalog: &Catalog, published: &Rates, f: &Flags) -> Cmd {
    println!(
        "spread seeds={} pulls_each={} NON-GATE diagnostic",
        f.seeds, f.pulls
    );
    let mut worst = 0u64;
    let mut chi2_max = 0f64;
    let mut min_ssr = u64::MAX;
    let mut max_ssr = 0u64;
    let mut over = 0u64;
    // 汇总不是"平均各 seed 的偏差"，而是把计数直接加起来再跑同一条门禁：
    // K×N 抽合起来就是 K×N 次独立试验，`Fit` 的 SD 自动按总抽数算，判读不用换算。
    let mut pooled = [0u64; 4];
    for k in 1..=f.seeds {
        let obs = simulate(params, catalog, k, f.pulls).map_err(|e| fail(&e.to_string()))?;
        let fit = Fit::new(obs.counts, obs.pulls, published);
        println!(
            "  seed={} dev_N={} dev_R={} dev_SR={} dev_SSR={} worst={} chi2={:.4}",
            k,
            fit.dev_millionths[0],
            fit.dev_millionths[1],
            fit.dev_millionths[2],
            fit.dev_millionths[3],
            fit.worst_dev(),
            fit.chi2(),
        );
        worst = worst.max(fit.worst_dev());
        chi2_max = chi2_max.max(fit.chi2());
        min_ssr = min_ssr.min(obs.counts[Rarity::SSR.index()]);
        max_ssr = max_ssr.max(obs.counts[Rarity::SSR.index()]);
        if !fit.dev_ok() {
            over += 1;
        }
        for (t, c) in obs.counts.iter().enumerate() {
            pooled[t] += c;
        }
    }
    let total = f.pulls * f.seeds;
    let pool = Fit::new(pooled, total, published);
    println!(
        "spread pooled pulls={total} dev_N={} dev_R={} dev_SR={} dev_SSR={} worst={} gate={MAX_DEV_MILLIONTHS} chi2={:.4}",
        pool.dev_millionths[0],
        pool.dev_millionths[1],
        pool.dev_millionths[2],
        pool.dev_millionths[3],
        pool.worst_dev(),
        pool.chi2(),
    );
    // 二项 SD 只用来判读跨度（float 不进任何门禁）。
    let expected = f.pulls * published.ssr as u64 / 1_000_000;
    let p = published.ssr as f64 / 1e6;
    let sigma = (f.pulls as f64 * p * (1.0 - p)).sqrt();
    println!(
        "spread per_seed_worst={worst} chi2_max={chi2_max:.4} seeds_over_gate={over}/{} ssr_span={min_ssr}..{max_ssr} expected={expected} sigma={sigma:.1} 跨度/SD={:.2}",
        f.seeds,
        (max_ssr.saturating_sub(min_ssr)) as f64 / sigma.max(1.0),
    );
    Ok(())
}

/// 门禁参照列的取法：**公示声明优先**（内嵌的那份，或 `--rates-file` 指定的那份）。
/// [`Published::check`] 会把声明与"参数现算的引擎输出"整份比对，于是改了参数没重跑
/// `catalog rates` 时 CLI 也是红的（不留"只有单测能发现"的缝）。
///
/// 只有当调用方显式要求别的视界（`--basis`）时才退回"按视界现算"，并且**必须打提示**：
/// 那一列此时比的是期望自洽，不再是"公示说的话"。
fn published_column(
    params: &GachaParams,
    basis: u64,
    explicit: Option<&Path>,
) -> Result<Rates, String> {
    let from_file;
    let text: &str = match explicit {
        Some(p) => {
            from_file = std::fs::read_to_string(p)
                .map_err(|e| format!("读公示文件 {} 失败: {e}", p.display()))?;
            &from_file
        }
        None => DEFAULT_RATES_JSON,
    };
    let decl = RatesFile::read(text)?;
    let effective = decl.check(params)?;
    if basis != decl.basis_pulls {
        eprintln!(
            "提示: --basis {basis} 与公示文件的视界 {} 不同 ⇒ 参照列改为按 {basis} 抽视界现算。\
             此时这一列只能证明期望自洽；要验公示请用默认视界（{}）。",
            decl.basis_pulls, decl.basis_pulls
        );
        return Ok(effective_rates(params, basis));
    }
    Ok(effective)
}

// ---- catalog ----

fn catalog(args: &[String]) -> Cmd {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub {
        "rates" => catalog_rates(&rest),
        "validate" => catalog_validate(&rest),
        other => Err(fail(&format!(
            "未知的 catalog 子命令: {other}（可用：rates, validate）"
        ))),
    }
}

fn catalog_rates(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let file = disclose::build(&GachaParams::V1, &catalog, f.basis).map_err(|e| fail(&e))?;
    let json = file.to_json();
    match &f.out {
        Some(p) => {
            // `--out rates.json`（裸文件名）的 parent 是 Some("")，对它 create_dir_all 会失败。
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))
                    .map_err(|e| fail(&e))?;
            }
            std::fs::write(p, &json).map_err(|e| fail(&format!("写 {} 失败: {e}", p.display())))?;
            println!(
                "catalog rates: 写入 {}（{} 字节，basis_pulls={}，items={}）",
                p.display(),
                json.len(),
                f.basis,
                file.items.len()
            );
        }
        None => print!("{json}"),
    }
    println!(
        "base N={} R={} SR={} SSR={} ({} {} {} {})",
        file.base.n,
        file.base.r,
        file.base.sr,
        file.base.ssr,
        file.base_percent.n,
        file.base_percent.r,
        file.base_percent.sr,
        file.base_percent.ssr,
    );
    println!(
        "effective N={} R={} SR={} SSR={} ({} {} {} {}) method={}",
        file.effective.n,
        file.effective.r,
        file.effective.sr,
        file.effective.ssr,
        file.effective_percent.n,
        file.effective_percent.r,
        file.effective_percent.sr,
        file.effective_percent.ssr,
        file.method
    );
    Ok(())
}

fn catalog_validate(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    println!(
        "catalog validate: id={} banner={} items={} pets={} accessories={}",
        catalog.id,
        catalog.banner_id,
        catalog.items.len(),
        catalog.pets().count(),
        catalog.accessories().count()
    );
    for r in Rarity::ALL {
        println!(
            "  {r:<3} items={} total_weight={}",
            catalog.items_of(r).count(),
            catalog.total_weight(r)
        );
    }
    if f.strict_assets {
        let mut missing = 0usize;
        for item in &catalog.items {
            match &item.manifest_path {
                Some(p) if !Path::new(p).exists() => {
                    println!("  MISSING {} → {p}", item.id);
                    missing += 1;
                }
                None => {
                    println!("  NO_MANIFEST {}", item.id);
                    missing += 1;
                }
                _ => {}
            }
        }
        if missing > 0 {
            return Err(fail(&format!("{missing} 件物品缺可用资产")));
        }
    }
    println!("catalog validate: OK");
    Ok(())
}

// ---- pull / redeem ----

fn open_store(f: &Flags) -> Result<Store, ExitCode> {
    let db = DbPath::resolve(f.db.as_deref());
    let allowed = f.allow_default || std::env::var_os(petstore::db::ALLOW_DEFAULT_ENV).is_some();
    let mut store = Store::open_with(&db, allowed).map_err(store_fail)?;
    if f.seed_explicit {
        // 换 seed 必须连带清账本（旧链的哈希与新 seed 对不上），所以这条是破坏性操作，
        // 不给 `--allow-reseed` 就拒绝 —— 绝不能让人以为"--seed 只是复放一次"。
        if !f.allow_reseed {
            return Err(fail(
                "--seed 会删除 gacha_pull / inventory / shards（换 seed 后旧审计链必然验不过）。 \
                 确实要重开一条链请加 --allow-reseed。",
            ));
        }
        store
            .reseed(f.seed)
            .map_err(|e| fail(&format!("重播种失败: {e}")))?;
    }
    Ok(store)
}

fn store_fail(e: StoreError) -> ExitCode {
    fail(&e.to_string())
}

fn pull(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    if let Some(raw) = &f.currency {
        return Err(refuse_currency(raw));
    }
    if f.count == 0 {
        return Err(fail("--count 至少 1"));
    }
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let params = GachaParams::V1;

    if f.dry_run {
        // 不碰磁盘、不扣币：只把"公示说了什么、引擎会给出什么"打出来（§10.2:949）。
        let published =
            published_column(&params, f.basis, f.rates_file.as_deref()).map_err(|e| fail(&e))?;
        let mut store = Store::in_memory().map_err(store_fail)?;
        if f.seed_explicit {
            // 内存库刚建、历史为空 ⇒ `--dry-run` 给 `--seed` 就是"照着这个 seed 复演一遍"，
            // 没有可丢的东西，所以这里不需要 `--allow-reseed`（真库上那条守卫照旧生效）。
            store.reseed(f.seed).map_err(store_fail)?;
        }
        store
            .install_catalog(&catalog)
            .map_err(|e| fail(&format!("装目录失败: {e}")))?;
        println!(
            "pull --dry-run banner={} count={} 内存库：不写盘、不扣币、进程退出即消失",
            f.banner, f.count
        );
        println!(
            "  公示 base N={} R={} SR={} SSR={} / effective N={} R={} SR={} SSR={}",
            params.rates.n,
            params.rates.r,
            params.rates.sr,
            params.rates.ssr,
            published.n,
            published.r,
            published.sr,
            published.ssr
        );
        let report = store
            .pull_for(&params, &catalog, &f.banner, &f.account, f.count)
            .map_err(store_fail)?;
        for line in report.lines() {
            println!("  {line}");
        }
        println!(
            "dry_run count={} N={} R={} SR={} SSR={} dup={} shards=+{} pity={} seed={}",
            f.count,
            report.count(Rarity::N),
            report.count(Rarity::R),
            report.count(Rarity::SR),
            report.count(Rarity::SSR),
            report.duplicates,
            report.shards_gained,
            report.outcomes.last().map(|o| o.pity_before).unwrap_or(0),
            store.seed()
        );
        return Ok(());
    }

    let mut store = open_store(&f)?;
    store
        .install_catalog(&catalog)
        .map_err(|e| fail(&format!("装目录失败: {e}")))?;
    let started = std::time::Instant::now();
    let report = store
        .pull_for(&params, &catalog, &f.banner, &f.account, f.count)
        .map_err(store_fail)?;
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    for line in report.lines() {
        println!("{line}");
    }
    println!(
        "pull banner={} account={} count={} seq={}..{} N={} R={} SR={} SSR={} dup={} shards=+{} balance={}",
        report.banner_id,
        report.profile_id,
        f.count,
        report.first_seq,
        report.last_seq,
        report.count(Rarity::N),
        report.count(Rarity::R),
        report.count(Rarity::SR),
        report.count(Rarity::SSR),
        report.duplicates,
        report.shards_gained,
        report.shards_after,
    );
    println!(
        "pull chain_len={} head={} seed={}",
        report.chain_len,
        report.head_hex,
        store.seed()
    );
    // §6.1"抽取不阻塞渲染"的口径：含 DB 写 + 双哈希的墙钟，**不含**建库迁移与装目录
    //（那是一次性的，混进来就不是抽取成本了）。
    let gate_ms = if f.count == 1 { 1.0 } else { 10.0 };
    let cold = report.chain_len == f.count as u64;
    let gate_ok = ms <= gate_ms;
    println!(
        "latency_ms={:.3} per_pull_ms={:.3} gate_ms={gate_ms} ok={} cold_start={cold} \
         (首库那一次带 WAL 建文件与 fsync，不作门禁判定，只报数)",
        ms,
        ms / f.count as f64,
        gate_ok || cold,
    );
    if f.verbose {
        let rep = store
            .verify(&catalog, &report.profile_id, &report.banner_id)
            .map_err(store_fail)?;
        println!("verify len={} head={}", rep.len, hex(&rep.head));
    }
    if gate_ok || cold {
        Ok(())
    } else {
        Err(ExitCode::FAILURE)
    }
}

fn redeem(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    let item = f
        .item
        .clone()
        .ok_or_else(|| fail("redeem 需要 --item ID"))?;
    let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
    let mut store = open_store(&f)?;
    store
        .install_catalog(&catalog)
        .map_err(|e| fail(&format!("装目录失败: {e}")))?;
    let balance = store
        .redeem_ssr(&GachaParams::V1, &catalog, item.as_str())
        .map_err(store_fail)?;
    println!(
        "redeem item={item} 碎片刻余={balance}（兑换不掷骰 ⇒ 不进 gacha_pull，也不进审计链；库存记 obtained_from='redeem:shards'）"
    );
    Ok(())
}

// ---- audit ----

fn audit(args: &[String]) -> Cmd {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub {
        "verify" => audit_verify(&rest),
        "export" => audit_export(&rest),
        other => Err(fail(&format!(
            "未知的 audit 子命令: {other}（可用：verify, export）"
        ))),
    }
}

fn audit_verify(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    match (&f.file, &f.db) {
        (Some(path), _) => {
            // 第三方路径：只吃导出文件 + seed + 公示参数，完全不碰数据库。
            if !f.seed_explicit {
                return Err(fail(
                    "重放的输入是 (seed, seq)：--file 必须同时给 --seed S（库里的 seed 不适用，\
                     你正在验的就是'别人给的账本'）。",
                ));
            }
            if f.count_explicit {
                return Err(fail(
                    "--count 只对 pull 有意义；audit verify 验的是文件里的全部记录。",
                ));
            }
            let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
            verify_export(path, &f, &GachaParams::V1, &catalog)
        }
        (None, _) => {
            let catalog = load_catalog(f.catalog.as_deref()).map_err(|e| fail(&e))?;
            let store = open_store(&f)?;
            let rep = store
                .verify(&catalog, &f.account, &f.banner)
                .map_err(store_fail)?;
            print_report("db", &rep, &f, &store);
            Ok(())
        }
    }
}

fn verify_export(path: &Path, f: &Flags, params: &GachaParams, catalog: &Catalog) -> Cmd {
    let text = std::fs::read_to_string(path)
        .map_err(|e| fail(&format!("读 {} 失败: {e}", path.display())))?;
    let records =
        Store::records_from_jsonl(&text).map_err(|e| fail(&format!("导出文件不可解: {e}")))?;
    // 先验链（有没有被删行/改行），再重放（骰子结果与公开挑战值对不对得上）。
    //
    // ⚠️ **没给 `--expect-head` 时，删掉末尾若干行是查不出来的**：哈希链是"每条记住上一条
    // 的摘要"，截尾只是少了几个尾节点，剩下的部分照样自洽，重放也只按文件里的长度跑。
    // 所以第三方要么用 `--expect-head`（从别处拿到的锚点：`opet pull` 打印的
    // `head=`、或作者公布的 checksum），要么接受"这条防线只能证明没被改，不能证明没被删"。
    let rep = match &f.expect_head {
        Some(head) => {
            verify_chain_anchored(&records, head).map_err(|e| fail(&format!("链校验失败: {e}")))?
        }
        None => {
            if f.verbose {
                eprintln!(
                    "提示: 未给 --expect-head ⇒ 本次只验'没被改/没被删中间行'，\
                     截尾删行不在本次可检范围内。"
                );
            }
            verify_chain(&records).map_err(|e| fail(&format!("链校验失败: {e}")))?
        }
    };
    verify_replay(f.seed, params, catalog, &records)
        .map_err(|e| fail(&format!("重放不一致（骰子被动过的证据）: {e}")))?;
    println!(
        "audit verify mode=file file={} seed={} len={} head={} counts={} {} {} {} 重放一致",
        path.display(),
        f.seed,
        rep.len,
        hex(&rep.head),
        rep.counts[0],
        rep.counts[1],
        rep.counts[2],
        rep.counts[3],
    );
    Ok(())
}

fn print_report(mode: &str, rep: &ChainReport, f: &Flags, store: &Store) {
    println!(
        "audit verify mode={mode} account={} banner={} len={} head={} seed={}",
        f.account,
        f.banner,
        rep.len,
        hex(&rep.head),
        store.seed()
    );
    println!(
        "  counts N={} R={} SR={} SSR={}",
        rep.counts[0], rep.counts[1], rep.counts[2], rep.counts[3]
    );
    // pity 不建表、从账本推导，所以把推导结果打出来，供与 `pull --dry-run` 对账。
    if let Ok(rows) = store.rows(&f.account, &f.banner)
        && let Some(last) = rows.last()
    {
        let row: &PullRow = last;
        println!(
            "  末条 seq={} pity_before={} ssr_prob={} dice_used={}",
            row.seq, row.pity_before, row.ssr_prob, row.dice_used
        );
    }
}

fn audit_export(args: &[String]) -> Cmd {
    let f = parse_flags(args).map_err(|e| fail(&e))?;
    let out = f
        .out
        .clone()
        .ok_or_else(|| fail("audit export 需要 --out FILE"))?;
    let store = open_store(&f)?;
    let text = store
        .export_jsonl(&f.account, &f.banner)
        .map_err(store_fail)?;
    std::fs::write(&out, &text).map_err(|e| fail(&format!("写 {} 失败: {e}", out.display())))?;
    println!(
        "audit export out={} bytes={} lines={}（第三方只需这个文件 + seed + 公示参数）",
        out.display(),
        text.len(),
        text.lines().count()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `--seed` 必须**逐位**进 `u64`：走 f64 的 `num()` 只有 53 位有效位，会把一个真账本
    /// 重放成另一个哈希。不是假想敌 —— 实测 `7377030585196009423` 经 f64 变成 `…472`，
    /// 于是 `audit verify --file` 会把没被动过的账本报成"骰子被动过"。
    #[test]
    fn seed_is_parsed_exactly_beyond_the_float_mantissa() {
        let args = sv(&["--seed", "7377030585196009423"]);
        let mut i = 0;
        let got = ints(&args, &mut i, "--seed").unwrap();
        assert_eq!(got, 7377030585196009423);
        assert_eq!(i, 1, "游标停在**值**上：调用方的 `i += 1` 才会跳过它");
        assert_ne!(
            got as f64 as u64, got,
            "这个 seed 本该超出 53 位有效位 —— 否则本测试什么也没证明"
        );
    }

    #[test]
    fn a_non_integer_or_missing_option_value_is_an_error_not_a_default() {
        for bad in ["1.5", "-1", "0x10", "", "73770305851960094230", "１２"] {
            let args = sv(&["--seed", bad]);
            let mut i = 0;
            assert!(
                ints(&args, &mut i, "--seed").is_err(),
                "{bad:?} 本该被拒，而不是被悄悄接进 u64 或截断"
            );
        }
        let args = sv(&["--seed"]);
        let mut i = 0;
        assert!(ints(&args, &mut i, "--seed").is_err(), "缺值也要报错");
    }

    /// 红线 4 的 CLI 落点：`--currency` **在任何 IO 之前**就被挡下。
    /// 这条测试的价值在于它不依赖数据库 —— 传什么都不该建库、不该掷骰。
    #[test]
    fn currency_flag_is_refused_before_anything_is_opened() {
        for bad in ["real_money", "cash", "usd", "CNY"] {
            let code = dispatch_inner("pull", &sv(&["--currency", bad, "--count", "1"]));
            assert_eq!(code, Err(ExitCode::FAILURE), "{bad} 本该被红线挡下");
        }
        // 连合法的软通货名字也不接受：这个参数根本不该存在（存在就会迟早被别处调用）。
        let ok = parse_flags(&sv(&["--currency", "shards"])).unwrap();
        assert_eq!(ok.currency.as_deref(), Some("shards"));
        assert_eq!(
            dispatch_inner("pull", &sv(&["--currency", "shards"])),
            Err(ExitCode::FAILURE),
            "--currency 一旦出现就必须失败，不论它的值是什么"
        );
    }

    /// `--file` 这一支的入口条件：重放的输入是 `(seed, seq)`，所以没 seed 就没法验；
    /// `--count` 在这里没有意义（验的是文件里的全部记录）。
    /// 两条都必须在**读文件之前**判定 ⇒ 用的路径可以不存在。
    #[test]
    fn file_verification_demands_the_seed_it_will_replay_with() {
        let no_seed = sv(&["--file", "/tmp/opet_definitely_absent.jsonl"]);
        assert_eq!(
            audit_verify(&no_seed),
            Err(ExitCode::FAILURE),
            "不给 --seed 就不该去读文件"
        );
        let with_count = sv(&[
            "--file",
            "/tmp/opet_definitely_absent.jsonl",
            "--seed",
            "1",
            "--count",
            "5",
        ]);
        assert_eq!(audit_verify(&with_count), Err(ExitCode::FAILURE));
    }

    #[test]
    fn expect_head_flag_absence_is_not_a_silent_pass() {
        let f = parse_flags(&sv(&[
            "--expect-head",
            "4d6a4bad4dc180fd7eeebde3b864c69e1b4e5f0c616c62c061ca8dd67664b51c",
        ]))
        .unwrap();
        assert!(f.expect_head.is_some());
        assert!(
            parse_flags(&sv(&["--expect-head"])).is_err(),
            "缺值的 --expect-head 要报错，不能当成'没给锚点'而退回到查不出截尾的那条路"
        );
    }
}
