//! 内容包安装器（FLOW §7 的那七步，P1）。
//!
//! **为什么逻辑在 `petstore` 而不是 `opet-cli`**：装包这件事的三条判据（"装完能在资产根里看到、
//! 库存里有对应行"、"半途失败的坏包一个字都不留"、"verify 不解压就能报出缺哪件"）都是**可判定**
//! 的，要能被测试钉住。而 `opet-cli` 没有 dev-dependencies，往它里面塞文件系统断言等于给仓库
//! 添一条新依赖（D8 的 allowlist 要为它核许可证原文）。更关键的是 [`crate::paths::AssetRoot::resolve_in`]
//! 收的是**注入的 [`Env`]**：把安装器写成"读进程环境"的形状，它就只能在真用户的 `$XDG_DATA_HOME`
//! 上做实验 —— 那正是本项目一路在躲的那类测试。
//!
//! 四条立住的口径，改代码前先读：
//!
//! 1. **所有门禁都在任何写盘之前**（方法论 = "拒绝要放在最便宜的一侧"）。包不合格 ⇒ 文件系统
//!    与数据库都不动。库不合格（`--db` 缺省而路径来自默认解析）⇒ 连包都不开。
//! 2. **原子落盘 = 先解到 `<root>/.tmp.<pack_id>/`，全部门禁过，再逐项目录 `rename` 进正式位置**。
//!    暂存目录与正式目录在**同一个**资产根里 ⇒ `rename` 必然同文件系统，不需要"复制再删"那种假原子。
//!    `AssetDir::list()` 跳过点开头的条目 ⇒ 暂存中的半包对加载器不可见。
//! 3. **归属清单 `<root>/.installed_by/<pack_id>`**：卸载只删这个清单里列的文件，目录按
//!    "最深先、只删空目录"剪，**绝不整目录 `remove_dir_all`**（FLOW §7 明写）。没有归属清单
//!    就没有安全的卸载 —— `rmdir` 失败（目录非空）才是"别人也在这里"的正确信号。
//! 4. **"落像素"与"给所有权"是两条规则**（用户 2026-11-05 拍板，同一轮把 §3.11.5 读准了）：
//!    **付费包**携带池内件 ⇒ 整包拒绝（[`is_paid`] + [`PackCheck::forbidden`]，判在最便宜的一侧，
//!    文件系统与账本都不动）；**免费包**携带池内件 ⇒ 像素照落、所有权一行不给
//!    （[`InstallOutcome::pixels_only`]，因为所有权只能来自抽/兑换 —— 否则一条 `install` 把整池送完）。
//!    账本侧 [`Store::grant_pack_items`] 另外**绝对**拒绝池内件：两处不是冗余，磁盘侧要放行官方
//!    像素包（否则 FLOW §3 的 INV 门禁永远绿不了），写入点那条才是"不能白送所有权"的真正闸门。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use petcore::assets::source::AssetArchive;
use petcore::assets::{
    AssetDir, AssetSource, builtin_dir, installed_dir, manifest_member, pack_prefix,
};
use petcore::gacha::{Catalog, ItemKind};

use crate::db::{PackGrant, Store};
use crate::licenses::{ContentPack, Entitlement, record_entitlement, register_pack};
use crate::paths::{AssetRoot, Env};

/// 单包体积上限（§3.11.1）。判的是**解出来的字节总和**，不是 zip 文件本身的大小 ——
/// store-only 让两者几乎相等，但"按打包者声明判"是 zip bomb 的经典漏法。
pub const MAX_PACK_MB: u64 = 64;

/// 包内那份清单的成员名（§3.11.1）。
pub const MANIFEST_MEMBER: &str = "manifest.toml";

/// 暂存目录的前缀。点开头 ⇒ 加载器不会把它当资产（模块文档第 2 条）。
pub const TMP_PREFIX: &str = ".tmp.";

/// 归属清单所在目录（相对资产根）。
pub const OWNER_DIR: &str = ".installed_by";

/// 一个**已通过结构门禁**的包：zip 能开、体积合规、`manifest.toml` 合法、包内布局与
/// 清单声明一致。拿到这个类型 = FLOW §7 的第 1–4 步已经做完。
#[derive(Debug)]
pub struct Pack {
    pub path: PathBuf,
    pub manifest: ContentPack,
    archive: AssetArchive,
    /// 包内真实存在的资产，`(kind, id)`，按 `(kind.as_str(), id)` 升序。
    pub items: Vec<(ItemKind, String)>,
    /// 解出来的字节总数（不含目录条目）。
    pub bytes: u64,
    pub entry_count: usize,
}

impl Pack {
    /// FLOW §7 第 1–4 步：开包 → 体积 → 清单 → 一致性。
    ///
    /// `max_mb` 是给**测试**留的口子（造一个超限包比造 64 MB 快），不是给用户关门禁用的
    /// ⇒ CLI 侧只接受**更大**的值，见 `--max-mb` 的拒绝分支。
    pub fn open(path: &Path, max_mb: u64) -> Result<Self, String> {
        // 第 2 步（symlink / 绝对路径 / `..` / deflate）在 `open` 这一刻整体出局，一条都不放过：
        // 跳过坏条目等于"装了一个我们不知道长什么样的包"。
        let archive = AssetArchive::open(path).map_err(|e| e.message())?;

        let limit = max_mb.saturating_mul(1024 * 1024);
        let mut bytes = 0u64;
        let mut entry_count = 0usize;
        for e in archive.entries() {
            if e.is_dir {
                continue;
            }
            entry_count += 1;
            bytes += e.len;
        }
        if bytes > limit {
            return Err(format!(
                "包解出来约 {} MiB（{bytes} B）超过上限 {max_mb} MiB ⇒ 整包拒绝（§3.11.1）",
                (bytes as f64 / (1024.0 * 1024.0)).ceil()
            ));
        }

        // 第 1 步：`manifest.toml`。缺这个成员 = 这不是我们的包。
        let raw = archive
            .read(MANIFEST_MEMBER)
            .map_err(|e| format!("读 {MANIFEST_MEMBER} 失败: {}", e.message()))?;
        let text = String::from_utf8(raw).map_err(|_| {
            format!("{MANIFEST_MEMBER} 不是合法 UTF-8（条目名或字段里混进了二进制）")
        })?;
        let manifest = ContentPack::from_toml(&text)?;
        manifest.validate()?;

        // 包内布局（`pets/<id>/pet.json`、`accessories/<id>/accessory.json`）是 kind 的**唯一**
        // 事实源：清单里的 id 只写字母名，不带 kind 前缀（`ContentPack.items` 的既有形状）。
        let mut items: Vec<(ItemKind, String)> = Vec::new();
        let mut two_kinds: Vec<String> = Vec::new();
        for r in archive.list() {
            let item = r.map_err(|e| format!("包内有条目解析不了: {}", e.message()))?;
            if let Some((prev, _)) = items.iter().find(|(_, id)| *id == item.id) {
                if *prev != item.kind {
                    two_kinds.push(item.id.clone());
                }
                continue;
            }
            items.push((item.kind, item.id));
        }
        if !two_kinds.is_empty() {
            two_kinds.sort();
            return Err(format!(
                "同一个 id 在 `pets/` 与 `accessories/` 下各有一份: {two_kinds:?} ⇒ 所有权模糊，整包拒绝"
            ));
        }
        items.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));

        // 声明与实物必须一字不差地互相印证。不印证就会长出两个事实源：清单说三件、包里两件
        // ⇒ 库存里那一行对应的像素不存在（§9.2 第 20 条那个空列表的形状）。
        let declared: BTreeSet<&str> = manifest.items.iter().map(String::as_str).collect();
        let actual: BTreeSet<&str> = items.iter().map(|(_, id)| id.as_str()).collect();
        let absent: Vec<&str> = declared
            .iter()
            .copied()
            .filter(|d| !actual.contains(d))
            .collect();
        let extra: Vec<&str> = actual
            .iter()
            .copied()
            .filter(|a| !declared.contains(a))
            .collect();
        if !absent.is_empty() || !extra.is_empty() {
            return Err(format!(
                "`{MANIFEST_MEMBER}` 的 items 与包内实物不一致：清单有而包里没有 {absent:?}；\
                 包里有而清单没写 {extra:?} ⇒ 整包拒绝"
            ));
        }

        // 声明的 `kind` 与实际带的那一类对得上（`bundle` 是唯一允许混的形状）。
        let mismatched: Vec<String> = items
            .iter()
            .filter(|(k, _)| match manifest.kind.as_str() {
                "pets" => *k != ItemKind::Pet,
                "accessories" => *k != ItemKind::Accessory,
                _ => false,
            })
            .map(|(k, id)| format!("{}/{id}", k.as_str()))
            .collect();
        if !mismatched.is_empty() {
            return Err(format!(
                "包声明 kind = {:?} 却带着别的内容: {mismatched:?}",
                manifest.kind
            ));
        }

        // `pack_path` 记的是**装完之后这个包文件在这台机器上的位置** ⇒ 以调用方给的路径为准，
        // 清单里那份不采信（它可能是发布机上写的相对路径）。包文件不复制：装的是解出来的资产，
        // 包留在原地（§3.11.1：packs 目录 ≠ 资产目录）。
        let mut manifest = manifest;
        manifest.pack_path = path.display().to_string();

        Ok(Self {
            path: path.to_path_buf(),
            manifest,
            archive,
            items,
            bytes,
            entry_count,
        })
    }

    /// 包内某个前缀下的全部条目名（升序，**含**目录条目 —— [`Self::files_of`] 才筛掉它们）。
    pub fn names_under(&self, kind: ItemKind, id: &str) -> Vec<String> {
        let prefix = item_prefix(kind, id);
        let mut v: Vec<String> = self
            .archive
            .entries()
            .iter()
            .map(|e| e.name.clone())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        v.sort();
        v
    }

    /// 某一项的全部**文件**条目名（去目录、去零长相对段），升序。
    pub fn files_of(&self, kind: ItemKind, id: &str) -> Vec<String> {
        let prefix = item_prefix(kind, id);
        self.names_under(kind, id)
            .into_iter()
            .filter(|n| n.len() > prefix.len() && !n.ends_with('/'))
            .collect()
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        self.archive
            .read(name)
            .map_err(|e| format!("读包内 {name} 失败: {}", e.message()))
    }

    /// 逐件预检（FLOW §7 第 7 步与 `verify` 的共用内核）。**完全只读**：不解压、不写盘、不碰库。
    pub fn check(&self, catalog: &Catalog) -> PackCheck {
        let items = self
            .items
            .iter()
            .map(|(kind, id)| self.check_one(*kind, id))
            .collect::<Vec<_>>();
        // "包里的件同时在随机池里"是一个**事实**，两种包各有自己的处理方式（用户 11-05 拍板）：
        // 免费包带池内件 = 官方像素包 ⇒ 允许，但**只落像素、不给所有权**（所有权只能靠抽/兑换，
        // 否则一条 `install` 就把整池送完了）；付费包带池内件 = 违反 §3.11.5 ⇒ 整包拒绝。
        let mut in_pool: Vec<String> = items
            .iter()
            .filter(|c| catalog.find(&c.id).is_some())
            .map(|c| format!("{}/{}", c.kind.as_str(), c.id))
            .collect();
        in_pool.sort();
        let forbidden = if is_paid(&self.manifest) {
            in_pool.clone()
        } else {
            Vec::new()
        };
        PackCheck {
            items,
            in_pool,
            forbidden,
        }
    }

    fn check_one(&self, kind: ItemKind, id: &str) -> ItemCheck {
        let mut problems = Vec::new();
        // 走 `AssetItem::manifest` 而不是自己拼名字：清单成员名只有 `manifest_member` 一份，
        // 而"清单里写的 sheet 叫什么"必须由解析出来的清单说 ⇒ 两处各拼一遍就会与 `pack.py` 漂移。
        let handle = self
            .archive
            .locate(kind, id)
            .and_then(|item| item.manifest().ok());
        match handle {
            None => problems.push(format!(
                "包里没有 {}{}/{}",
                pack_prefix(kind),
                id,
                manifest_member(kind)
            )),
            Some(h) => match h.read_text() {
                Err(e) => problems.push(format!("清单读不出来: {}", e.message())),
                Ok(text) => match sheet_of(kind, &text) {
                    Err(e) => problems.push(e),
                    Ok(sheet) => {
                        let member = format!("{}{id}/{sheet}", pack_prefix(kind));
                        if !self.names_under(kind, id).contains(&member) {
                            problems.push(format!("清单说 sheet 是 {sheet}，包里没有这个成员"));
                        }
                    }
                },
            },
        }
        ItemCheck {
            kind,
            id: id.to_string(),
            problems,
        }
    }
}

/// 一项资产在包内的那一段前缀，形如 `accessories/hat_red/`。
fn item_prefix(kind: ItemKind, id: &str) -> String {
    format!("{}{id}/", pack_prefix(kind))
}

/// 清单文本 → 那份清单声明的 sheet 成员名。**`verify`（读包内）与 INV 门禁（读盘上）共用这一把尺**：
/// "能不能被加载器解析"这件事只有一处定义，于是"包验得过、装完 INV 却红"不可能出在解析这一步。
/// （两边**各自**查成员存在性，那是必须的差别：包内查的是 zip 索引，盘上查的是 `is_file()`。）
fn sheet_of(kind: ItemKind, text: &str) -> Result<String, String> {
    match kind {
        ItemKind::Pet => petcore::assets::Manifest::parse(text)
            .map(|m| m.sheet)
            .map_err(|e| format!("清单解析失败: {e}")),
        ItemKind::Accessory => petcore::assets::AccessoryManifest::parse(text)
            .map(|m| m.sheet)
            .map_err(|e| format!("清单解析失败: {e}")),
    }
}

/// 一件资产的预检结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCheck {
    pub kind: ItemKind,
    pub id: String,
    /// 空 = 这一件可被加载器解析。**每条都要能直接打给用户看**，不是内部码。
    pub problems: Vec<String>,
}

impl ItemCheck {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// [`Pack::check`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackCheck {
    pub items: Vec<ItemCheck>,
    /// 同时出现在随机池里的那些 `"kind/id"`。⚠️ **这一批不是错误** —— 它是"只落像素、不给所有权"
    /// 的那一批（免费包）。真正要拒绝的看 [`Self::forbidden`]。
    pub in_pool: Vec<String>,
    /// **付费包**携带池内件 ⇒ 违反 §3.11.5 ⇒ 整包拒绝（`install` 与 `verify` 都以这一条判红）。
    /// 免费包这一条恒空，`in_pool` 却有内容 —— 分开写是为了让"拒绝"只有一处能被触发。
    pub forbidden: Vec<String>,
}

impl PackCheck {
    pub fn bad_items(&self) -> usize {
        self.items.iter().filter(|i| !i.is_ok()).count()
    }

    /// 一句能直接打的结论（`verify` 与 `install` 共用，保证两处措辞不漂移）。
    pub fn summary(&self) -> String {
        format!(
            "items={} bad={} in_pool={} forbidden={}",
            self.items.len(),
            self.bad_items(),
            self.in_pool.len(),
            self.forbidden.len()
        )
    }
}

/// 一类资产在本次安装里要动的那两处：资产根 + 它的暂存与归属清单。
struct Target {
    kind: ItemKind,
    root: AssetRoot,
    tmp: PathBuf,
    owner: PathBuf,
    /// 本根要落地的项 id。
    ids: Vec<String>,
    /// 相对本根的落盘文件清单（写进 owner 文件的那份）。
    files: Vec<String>,
}

/// 装包所需的调用方选择。写成 struct 而不是三个参数：将来第六个字段一定是又一个 bool，
/// 而位置参数会把两个同型 bool 接错（本项目"两个 bool 反了"已经栽过）。
#[derive(Clone, Debug, Default)]
pub struct InstallOpts {
    /// 接管"资产根里已存在、但归属清单不认"的目录（默认拒绝）。
    pub force: bool,
    /// `--redeem CODE` ⇒ 权益来源记成 `redeem:CODE`；`None` 时按 `store` 推。
    pub redeem: Option<String>,
}

/// [`install`] 的回执。字段全是**给人打的一行**，不是内部状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstallOutcome {
    pub pack_id: String,
    pub title: String,
    /// 每个动过的资产根，`AssetRoot::describe()` 的形状。
    pub roots: Vec<String>,
    pub files: usize,
    pub bytes: u64,
    pub grant: PackGrant,
    /// `entitlements.source` 实际写了什么（一句话，含权益 id）。
    pub source: String,
    /// 第 4 步：`signature` 缺席 ⇒ 屏上要打"第三方未验证"（§3.11.2：这是特性不是漏洞）。
    pub unsigned: bool,
    /// 被 `--force` 从别人手里接管的项（`"kind/id"`）。
    pub took_over: Vec<String>,
    /// **只落像素、不给所有权**的那些件（`"kind/id"`）= 免费包里的池内件。
    /// 所有权只能来自抽/兑换（否则一条 `install` 把整池送完），但像素必须能随包分发，
    /// 否则 FLOW §3 的 INV 门禁永远绿不了。屏上要把这一批点名，不能当"装进了 N 件"混过去。
    pub pixels_only: Vec<String>,
}

/// 权益来源（`entitlements.source` 的四种形状之一）。单独一只函数是为了让它**可测**：
/// 明码标价的包被记成 `free`，等于账面上从来没卖过付费内容。
pub fn entitlement_source(manifest: &ContentPack, redeem: Option<&str>) -> String {
    match redeem {
        Some(code) => format!("redeem:{code}"),
        None if manifest.store == "free" => "free".into(),
        None => format!("purchase:{}#local", manifest.store),
    }
}

/// 这条包是不是**付费包**（§3.11.5 那句话的主语）。
///
/// 判价格而不是判 `store`：`ContentPack::validate()` 已经把"`store=free` ⇔ 价格必须为 0"绑死，
/// 所以两个判据等价；而"有没有收钱"是这条红线真正在意的那件事。
/// ⚠️ **判"付不付费"只许用这一只函数** —— 两处各写 `price_cents > 0` 就是两条事实源。
pub fn is_paid(manifest: &ContentPack) -> bool {
    manifest.price_cents.unwrap_or(0) > 0
}

/// FLOW §7 的全部七步。**全部门禁先跑完，才碰文件系统。**
///
/// 失败时的承诺（验收判据的字面实现）：**正式位置一个字都不留**。手段 = "先全部写进
/// `.tmp.<pack_id>/`，再逐项 `rename`"：`rename` 之前失败 ⇒ 正式位置从未被写；`rename`
/// 中途失败（权限、盘满）或**数据库写失败** ⇒ 按已建的归属清单删回去。
pub fn install(
    pack: &Pack,
    env: &Env,
    store: &mut Store,
    catalog: &Catalog,
    opts: &InstallOpts,
) -> Result<InstallOutcome, String> {
    let pack_id = pack.manifest.id.as_str();
    // 包 id 会被拼成目录名与归属清单文件名 ⇒ 先过一次路径安全判定（同一份规则只有
    // `zip::check_safe_name` 一个实现）。`grant_pack_items` 里还会再判一次：那是账本侧的
    // 写入点，两处不是冗余。
    petcore::assets::zip::check_safe_name(pack_id).map_err(|e| e.message())?;

    // ---- 门禁一：内容预检 + 付费包撞池。放在最前面 ⇒ 坏包连暂存目录都不会建。----
    let check = pack.check(catalog);
    if !check.forbidden.is_empty() {
        return Err(format!(
            "付费包内容 {:?} 同时在随机池里 ⇒ 违反 §3.11.5（付费包卖池外内容）。整包拒绝，\
             文件系统与账本都没动。",
            check.forbidden
        ));
    }
    let bad: Vec<String> = check
        .items
        .iter()
        .filter(|i| !i.is_ok())
        .map(|i| format!("{}: {}", i.id, i.problems.join("；")))
        .collect();
    if !bad.is_empty() {
        return Err(format!("包里 {} 件解析不了 ⇒ 整包拒绝：{bad:?}", bad.len()));
    }

    // ---- 门禁二：目标位置可归属判定（要在建暂存目录之前，理由同上）。----
    let mut targets: Vec<Target> = Vec::new();
    let mut took_over: Vec<String> = Vec::new();
    for kind in ItemKind::ALL {
        let ids: Vec<String> = pack
            .items
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, id)| id.clone())
            .collect();
        if ids.is_empty() {
            continue;
        }
        let root = AssetRoot::resolve_in(env, kind).ok_or_else(|| {
            format!(
                "这台机器解析不出 {} 的资产根（`$HOME` 与 `$XDG_DATA_HOME` 都读不到）⇒ 不猜一个根",
                AssetRoot::subdir(kind)
            )
        })?;
        let tmp = root.path.join(format!("{TMP_PREFIX}{pack_id}"));
        let owner = root.path.join(OWNER_DIR).join(pack_id);
        let owned: BTreeSet<String> = if owner.is_file() {
            read_owner_list(&owner)?.into_iter().collect()
        } else {
            Default::default()
        };
        let mut conflicts: Vec<String> = Vec::new();
        for id in &ids {
            let dest = root.path.join(id);
            let ours = owned.iter().any(|rel| rel.starts_with(&format!("{id}/")));
            if dest.exists() && !ours {
                conflicts.push(format!("{}/{id}", kind.as_str()));
            }
        }
        if !conflicts.is_empty() && !opts.force {
            return Err(format!(
                "目标位置已有不属于本包的项: {conflicts:?} ⇒ 拒绝覆盖（手工塞进去的、或别的包装的）。\
                 确认要接管请加 --force。"
            ));
        }
        took_over.extend(conflicts);
        targets.push(Target {
            kind,
            root,
            tmp,
            owner,
            ids,
            files: Vec::new(),
        });
    }
    took_over.sort();
    // 池内件（此时只可能是**免费包**带的，付费包上面已经拒了）：像素照落，所有权一行都不给。
    // 判据与 `check` 用同一份 `in_pool` ⇒ 不会出现"报告说只落像素、账上却写了所有权"。
    let mut pixels_only: Vec<String> = Vec::new();
    let mut grantable: Vec<(ItemKind, String)> = Vec::new();
    for (kind, id) in &pack.items {
        if check.in_pool.contains(&format!("{}/{}", kind.as_str(), id)) {
            pixels_only.push(format!("{}/{}", kind.as_str(), id));
        } else {
            grantable.push((*kind, id.clone()));
        }
    }
    pixels_only.sort();

    // ---- 到这里"该不该装"全部判完。下面才开始写。----
    let mut outcome = InstallOutcome {
        pack_id: pack_id.to_string(),
        title: pack.manifest.title.clone(),
        took_over,
        pixels_only,
        unsigned: pack.manifest.signature.is_none(),
        ..Default::default()
    };
    for t in targets.iter() {
        outcome.roots.push(t.root.describe());
    }

    // 暂存：全部字节先进 `.tmp.<pack_id>/`。这一段的任何失败都只删暂存 ⇒ 正式位置没被碰过。
    if let Err(e) = stage(pack, &mut targets) {
        for t in targets.iter() {
            let _ = std::fs::remove_dir_all(&t.tmp);
        }
        return Err(format!("解包失败（正式位置一个字未写）: {e}"));
    }

    // 落地：逐项目录 `rename`。已 `rename` 的记在 `landed` 里，失败就按它回滚。
    let mut landed: Vec<PathBuf> = Vec::new();
    if let Err(e) = commit(&mut targets, &mut landed) {
        for p in &landed {
            let _ = remove_any(p);
        }
        for t in targets.iter() {
            let _ = std::fs::remove_file(&t.owner);
            let _ = std::fs::remove_dir_all(&t.tmp);
        }
        return Err(format!("装包失败并已回滚（正式位置不留半个字）: {e}"));
    }
    // 暂存目录已由 `commit` 收尾删掉（它只剩空壳）⇒ 这里不再碰它。
    outcome.files = targets.iter().map(|t| t.files.len()).sum();
    outcome.bytes = pack.bytes;

    // ---- 文件系统已到位，最后动账。账写失败 ⇒ 回滚文件系统（两处不一致比装失败更糟）。----
    let source = entitlement_source(&pack.manifest, opts.redeem.as_deref());
    let ref_key = format!("local:{pack_id}");
    let ent = (|| -> Result<Entitlement, String> {
        register_pack(store.conn(), &pack.manifest)
            .map_err(|e| format!("登记 content_packs 失败: {e}"))?;
        record_entitlement(
            store.conn(),
            pack_id,
            &source,
            Some(&ref_key),
            crate::clock::now_secs(),
        )
        .map_err(|e| format!("记权益失败: {e}"))
    })();
    let ent = match ent {
        Ok(e) => e,
        Err(why) => {
            rollback_files(&targets, &landed);
            return Err(format!("写权益表失败并已回滚文件: {why}"));
        }
    };
    outcome.source = format!("{source}（权益 id {}）", ent.id);
    // 所有权只给**池外**的那些件。整包都是池内像素时（官方免费像素包就是这个形状）一行都不写，
    // 但也不去叫 `grant_pack_items` 拿空列表报"包里一件都没有"—— 那是另一回事。
    if grantable.is_empty() {
        outcome.grant = PackGrant::default();
    } else {
        match store.grant_pack_items(pack_id, catalog, &grantable) {
            Ok(grant) => outcome.grant = grant,
            Err(e) => {
                let why = e.to_string();
                rollback_files(&targets, &landed);
                return Err(format!("写库存失败并已回滚文件: {why}"));
            }
        }
    }
    Ok(outcome)
}

/// 把包内每一项解到它那个资产根的 `.tmp.<pack_id>/<id>/` 下，并填好 `Target::files`。
fn stage(pack: &Pack, targets: &mut [Target]) -> Result<(), String> {
    for t in targets.iter_mut() {
        // 上一轮崩在这里留下的暂存目录：按命名约定它就是本包的，先清掉再解。
        if t.tmp.exists() {
            std::fs::remove_dir_all(&t.tmp)
                .map_err(|e| format!("清理残留暂存 {} 失败: {e}", t.tmp.display()))?;
        }
        for id in &t.ids {
            for name in pack.files_of(t.kind, id) {
                let rel = name
                    .strip_prefix(&item_prefix(t.kind, id))
                    .ok_or_else(|| format!("包内条目 {name} 不在预期前缀下"))?;
                let dest = t.tmp.join(id).join(rel);
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)
                        .map_err(|e| format!("建暂存目录 {} 失败: {e}", p.display()))?;
                }
                std::fs::write(&dest, pack.read(&name)?)
                    .map_err(|e| format!("写暂存文件 {} 失败: {e}", dest.display()))?;
                t.files.push(format!("{id}/{rel}"));
            }
        }
        t.files.sort();
    }
    Ok(())
}

/// 逐项目录 `rename` 进正式位置，并写归属清单。`landed` 收集**已经放进正式位置**的目录，
/// 供调用方回滚（归属清单也在同一轮写好 ⇒ 回滚侧只认 `landed`，不去猜谁是谁的）。
fn commit(targets: &mut [Target], landed: &mut Vec<PathBuf>) -> Result<(), String> {
    for t in targets.iter_mut() {
        std::fs::create_dir_all(&t.root.path)
            .map_err(|e| format!("建资产根 {} 失败: {e}", t.root.path.display()))?;
        for id in &t.ids {
            let dest = t.root.path.join(id);
            if dest.exists() {
                // 只有 `--force` 接管或本包重装会走到这里（冲突已在门禁二拦掉）。
                remove_any(&dest)?;
            }
            std::fs::rename(t.tmp.join(id), &dest)
                .map_err(|e| format!("落地 {} 失败: {e}", dest.display()))?;
            landed.push(dest);
        }
        write_owner_list(&t.owner, &t.files)?;
        // 每个 `<id>/` 都已被 `rename` 走 ⇒ 暂存目录只剩自己这一层空壳，删掉它。
        // 删失败不计入本次成败（文件与账都已就位），但也**不用 `remove_dir_all`**：
        // 收尾这一步不该有能碰掉正式位置的力量。
        let _ = std::fs::remove_dir(&t.tmp);
    }
    Ok(())
}

/// 按归属清单删文件 + 剪空目录 + 删清单本身（数据库失败时的文件侧回滚）。
fn rollback_files(targets: &[Target], landed: &[PathBuf]) {
    for p in landed {
        let _ = remove_any(p);
    }
    for t in targets {
        let _ = std::fs::remove_file(&t.owner);
        let _ = std::fs::remove_dir_all(&t.tmp);
    }
}

fn remove_any(p: &Path) -> Result<(), String> {
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| format!("删 {} 失败: {e}", p.display()))
    } else {
        std::fs::remove_file(p).map_err(|e| format!("删 {} 失败: {e}", p.display()))
    }
}

/// 归属清单文件：一行一个**相对资产根**的路径，升序。写侧排序 = 读侧可以直接 diff，
/// 也让 `opet pack uninstall` 的删除顺序确定（硬约束 #8 的同族口径）。
fn write_owner_list(path: &Path, files: &[String]) -> Result<(), String> {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort();
    let mut text = String::new();
    for f in sorted {
        text.push_str(f);
        text.push('\n');
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("建 {} 失败: {e}", p.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写归属清单 {} 失败: {e}", path.display()))
}

fn read_owner_list(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读归属清单 {} 失败: {e}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// [`uninstall`] 的回执。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub pack_id: String,
    pub removed: usize,
    pub pruned_dirs: usize,
    pub inventory_rows: usize,
    /// 找过的资产根。一个都没找到时 `removed = 0` 要说清是"没装过"还是"装在别的根"。
    pub searched: Vec<String>,
    /// 删不动的文件（权限等）⇒ 留着并点名，绝不硬删。
    pub kept: Vec<String>,
}

/// 卸包：**只删归属清单里列的文件**，目录按"最深先、只删空目录"剪，然后删 `inventory` 里
/// 本包那几行。`content_packs` 与 `entitlements` **原样留着**（卸载 ≠ 退款：交易被收回是
/// [`crate::licenses::revoke`] 那条路，与"文件不在磁盘上"是两件事）。
pub fn uninstall(env: &Env, store: &mut Store, pack_id: &str) -> Result<UninstallOutcome, String> {
    petcore::assets::zip::check_safe_name(pack_id).map_err(|e| e.message())?;
    let mut out = UninstallOutcome {
        pack_id: pack_id.to_string(),
        ..Default::default()
    };
    let mut touched = false;
    for kind in ItemKind::ALL {
        let Some(root) = AssetRoot::resolve_in(env, kind) else {
            continue;
        };
        out.searched.push(root.describe());
        let owner = root.path.join(OWNER_DIR).join(pack_id);
        if !owner.is_file() {
            continue;
        }
        touched = true;
        let files = read_owner_list(&owner)?;
        // 最深的目录先剪：`a/b/c.png` 的父链要按 `a/b` → `a` 试，反过来 `a` 还非空。
        let mut dirs: Vec<PathBuf> = Vec::new();
        for rel in &files {
            let mut p = root.path.join(rel);
            while let Some(parent) = p.parent() {
                if parent == root.path {
                    break;
                }
                let parent = parent.to_path_buf();
                if !dirs.contains(&parent) {
                    dirs.push(parent.clone());
                }
                p = parent;
            }
        }
        for rel in &files {
            let p = root.path.join(rel);
            match std::fs::remove_file(&p) {
                Ok(()) => out.removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => out.kept.push(format!("{}: {e}", p.display())),
            }
        }
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for d in dirs {
            if std::fs::remove_dir(&d).is_ok() {
                out.pruned_dirs += 1;
            }
        }
        let _ = std::fs::remove_file(&owner);
        if let Some(dir) = owner.parent() {
            // `.installed_by` 空了才删；还挂着别的包的清单就留着（它不是本包的私有目录）。
            let _ = std::fs::remove_dir(dir);
        }
    }
    out.inventory_rows = store
        .revoke_pack_items(pack_id)
        .map_err(|e| e.to_string())?;
    if !touched && out.inventory_rows == 0 && out.kept.is_empty() {
        return Err(format!(
            "没有名为 {pack_id} 的已装包（查过 {} 个资产根，库存里也没有它的行）。\
             装了哪些包用 `opet pack list` 查。",
            out.searched.len()
        ));
    }
    Ok(out)
}

/// 资产根里现在有什么（`opet pack list` 的右半边）。
///
/// 存在的理由 = §9.2 第 20 条那个"两条路互不相认"：注册表（`content_packs` + `inventory`）
/// 与磁盘（资产根目录）必须在**同一个命令的输出里**对得上，否则"装没装"只能靠人脑 diff。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootListing {
    pub describe: String,
    /// 磁盘上可解析的项（`"kind/id"`）。
    pub items: Vec<String>,
    /// 磁盘上有目录但解析不了的项（点名，不静默跳过）。
    pub problems: Vec<String>,
}

pub fn scan_roots(env: &Env) -> Vec<RootListing> {
    let mut out = Vec::new();
    for kind in ItemKind::ALL {
        let Some(root) = AssetRoot::resolve_in(env, kind) else {
            out.push(RootListing {
                describe: format!("{}（解析不出根 ⇒ 这一类不存在）", AssetRoot::subdir(kind)),
                ..Default::default()
            });
            continue;
        };
        let dir: AssetDir = installed_dir(&root.path, kind);
        let mut listing = RootListing {
            describe: root.describe(),
            ..Default::default()
        };
        for r in dir.list() {
            match r {
                Ok(item) => listing
                    .items
                    .push(format!("{}/{}", item.kind.as_str(), item.id)),
                Err(e) => listing.problems.push(e.message().to_string()),
            }
        }
        out.push(listing);
    }
    out
}

/// 一条 `content_packs` 行 + 它给了库存几件（`opet pack list` 的左半边）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegisteredPack {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub store: String,
    pub price_cents: Option<i64>,
    pub pack_path: String,
    /// 库存里挂着 `obtained_from = 'pack:<id>'` 的行数。
    pub inventory_rows: usize,
}

pub fn registered(store: &Store) -> Result<Vec<RegisteredPack>, String> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT p.id, p.title, p.kind, p.store, p.price_cents, p.pack_path,
                    (SELECT COUNT(*) FROM inventory i
                      WHERE i.obtained_from = 'pack:' || p.id)
             FROM content_packs p ORDER BY p.id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RegisteredPack {
                id: r.get(0)?,
                title: r.get(1)?,
                kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                store: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                price_cents: r.get(4)?,
                pack_path: r.get(5)?,
                inventory_rows: r.get::<_, i64>(6)?.max(0) as usize,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

// ---- INV 门禁（FLOW §3 推论 2）----

/// [`inv_check`] 看到了哪些源，以及池内每件的判定。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InvReport {
    /// 查了哪些源（`label:路径`，按 `ItemKind::ALL` 顺序，每个 kind 内"内置在前、装机在后"）。
    /// 红的时候必须能说清"我查了哪儿"，否则"缺"读起来像"没查"。
    pub sources: Vec<String>,
    /// 池内每件，顺序 = catalog 里的顺序（`problems` 空 = 这一件合规）。
    pub items: Vec<ItemCheck>,
    /// 缺的那些该去装哪个包（由 catalog id 派生，§3.11.1 的包名口径）。
    pub pack_hint: String,
}

impl InvReport {
    pub fn bad_items(&self) -> usize {
        self.items.iter().filter(|i| !i.is_ok()).count()
    }

    pub fn ok(&self) -> bool {
        self.bad_items() == 0
    }

    /// 一句能直接打的结论（与 [`PackCheck::summary`] 同一族键名，CI 只 grep 一种写法）。
    pub fn summary(&self) -> String {
        let bad = self.bad_items();
        format!(
            "pool={} ok={} missing={}",
            self.items.len(),
            self.items.len() - bad,
            bad
        )
    }
}

/// **INV：抽取可能返回的每一件物品，其资产在抽取发生之前就必须完整存在于本地并可被加载器解析。**
/// （FLOW §3 原文；这条不变量是"抽到却穿不上"那个断链的正解，判定权在这里，打印格式在 CLI。）
///
/// 两件必须说清的事：
///
/// - **它必须能红。** 一台从没装过包的机器上池内**全红**，那是它在正常工作 —— 设计书与 FLOW 都
///   把"永远绿的门禁"登记为"假门禁"那一族。第一次跑红一大片不要当回归，也不要"修绿"它。
/// - **`builtin` 只服务开发态**：仓库那份 `assets/` 的 `<subdir>/<id>/` 布局与装机后的完全一致，
///   所以传进来就等价于"运行时会先看内置那批"。不传 ⇒ 只查装机后的根，结果随这台机器装过
///   什么包变 ⇒ 门禁量的正是"这台机器能不能安全开抽"。
pub fn inv_check(env: &Env, catalog: &Catalog, builtin: Option<&Path>) -> InvReport {
    let pack_hint = format!("{}.petpack", catalog.id);
    let mut dirs: Vec<(ItemKind, AssetDir)> = Vec::new();
    for kind in ItemKind::ALL {
        if let Some(root) = builtin {
            dirs.push((kind, builtin_dir(root.join(AssetRoot::subdir(kind)), kind)));
        }
        if let Some(r) = AssetRoot::resolve_in(env, kind) {
            dirs.push((kind, installed_dir(&r.path, kind)));
        }
    }
    let sources = dirs
        .iter()
        .map(|(_, d)| format!("{}:{}", d.label(), d.root().display()))
        .collect();
    let items = catalog
        .items
        .iter()
        .map(|it| {
            let mut hit: Option<ItemCheck> = None;
            for (kind, dir) in &dirs {
                if *kind != it.kind {
                    continue;
                }
                // `locate` 只对"目录在且清单在"给 Some ⇒ 下面先按命中的源判，没命中再解释为什么。
                let Some(item) = dir.locate(it.kind, &it.id) else {
                    continue;
                };
                let mut why = Vec::new();
                match item.manifest().and_then(|h| h.read_text()) {
                    Err(e) => why.push(format!("清单读不出来: {}", e.message())),
                    Ok(text) => match sheet_of(it.kind, &text) {
                        Err(e) => why.push(e),
                        Ok(sheet) => match item.member(&sheet) {
                            Err(e) => why.push(e.message().to_string()),
                            Ok(h) if h.exists() => {}
                            Ok(h) => why.push(format!(
                                "清单说 sheet 是 {sheet}，但这个文件不在：{}",
                                h.describe()
                            )),
                        },
                    },
                }
                hit = Some(ItemCheck {
                    kind: it.kind,
                    id: it.id.clone(),
                    problems: why,
                });
                break;
            }
            hit.unwrap_or_else(|| ItemCheck {
                kind: it.kind,
                id: it.id.clone(),
                problems: vec![if dirs
                    .iter()
                    .filter(|(k, _)| *k == it.kind)
                    .any(|(_, d)| d.root().join(&it.id).is_dir())
                {
                    // 区分"目录压根没有"与"目录有但清单不见/不合法"：前者是缺产物，
                    // 后者是产物坏了 —— 两者的修法完全不同。
                    format!(
                        "目录在，但清单 {}{}{} 读不出来",
                        pack_prefix(it.kind),
                        it.id,
                        manifest_member(it.kind)
                    )
                } else {
                    format!("任何一条源上都没有这一件 ⇒ 先装 {pack_hint}")
                }],
            })
        })
        .collect();
    InvReport {
        sources,
        items,
        pack_hint,
    }
}
