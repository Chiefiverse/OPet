//! 资产从哪儿来（设计书 §3.11.1 的 `AssetSource`，本轮才真的存在）。
//!
//! 一句话的职责：**给定 id，告诉调用方"这项资产在哪、怎么把字节读出来"，并且不泄漏它
//! 到底是磁盘目录还是 zip 里的条目。** §3.11.1 的原话是"宠物内核只见 `AssetId`，
//! 不知道它来自免费目录还是付费包" —— 上一版草图把这个 trait 写成 `-> Option<PathBuf>`，
//! 那是**在类型上就漏底**：zip 条目没有路径可给。所以这里对外只给 [`AssetHandle`]
//! （不透明句柄，能 `read()` 能 `describe()`，拿不到 `PathBuf`）。
//!
//! 三条边界，写之前先认：
//!
//! 1. **像素不归这里**（D12）。本模块只交字节，WebP 解码、抠格、写 shm 全在
//!    `overlay-wayland`。`atlas.rs` 今天就是这么加载的，改成"从 handle 拿字节"不动它的解码。
//! 2. **字节读取发生在装载那一刻，不在每帧**。`Equip` 一次读一个 sheet（几 MB），
//!    渲染循环仍然只碰内存里那份 ⇒ 与 §4"禁止定时轮询"、`fps` 分档那些事无关。
//! 3. **成员名是不可信输入**。`sheet` 字段写在清单里，而清单可以来自别人打的包 ⇒
//!    [`AssetItem::member`] 一律先过 [`crate::assets::zip::check_safe_name`]（同一条规则、
//!    同一个函数，不搞两份），`../../etc/passwd` 当名字用就是路径穿越。

use std::path::{Path, PathBuf};

use crate::gacha::catalog::ItemKind;

use super::zip::{self, ZipIndex};

/// 一个源里读不出内容时的错误。**消息必须点名到"哪个源的哪一项的哪个成员"**，
/// 因为换装页的错误报告面（FLOW §8）就是靠这句话区分"你没有这件"和"这件产物坏了"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetError(String);

impl AssetError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AssetError {}

/// 一份资产内容的**不透明句柄**。两个变体是"字节在哪儿"的两种方式，不是两种类型的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetHandle {
    /// 磁盘上的一个文件（内置目录 / 已装包的落地目录）。
    File(PathBuf),
    /// 未拆的 `.petpack` 里的一个条目：只记包文件与条目名，**读的时候再开一次索引**。
    /// 为什么不揣着 [`ZipIndex`]：① 句柄要能廉价 `Clone`/比较；② 每次读顺带重验一遍
    /// 中央目录与 CRC ⇒ 装载期间包被动过会当场报错，而不是用一份旧索引解出坏像素。
    Entry { pack: PathBuf, name: String },
}

impl AssetHandle {
    /// 给人看的一行：路径，或 `包!条目`。**只用于报告，不许拿去当路径用**（那是漏底）。
    pub fn describe(&self) -> String {
        match self {
            Self::File(p) => p.display().to_string(),
            Self::Entry { pack, name } => format!("{}!{name}", pack.display()),
        }
    }

    /// 读字节。**zip 那一支在这里验 CRC**（[`ZipIndex::read_entry`]），磁盘那一支没有
    /// 可信的校验和可比 ⇒ 只报 I/O 失败。
    pub fn read(&self) -> Result<Vec<u8>, AssetError> {
        match self {
            Self::File(p) => std::fs::read(p)
                .map_err(|e| AssetError::new(format!("读 {} 失败: {e}", p.display()))),
            Self::Entry { pack, name } => {
                let idx = ZipIndex::open(pack).map_err(|e| AssetError::new(e.message()))?;
                idx.read(name)
                    .map_err(|e| AssetError::new(format!("{}: {}", pack.display(), e.message())))
            }
        }
    }

    pub fn exists(&self) -> bool {
        match self {
            Self::File(p) => p.is_file(),
            // 句柄是从索引里造出来的，条目本就在索引里；这里查的是**包文件还在不在**。
            Self::Entry { pack, .. } => pack.is_file(),
        }
    }

    /// 文本成员（清单）读成 `String`。UTF-8 坏了一条也要点名是谁。
    pub fn read_text(&self) -> Result<String, AssetError> {
        let bytes = self.read()?;
        String::from_utf8(bytes)
            .map_err(|_| AssetError::new(format!("{} 不是合法 UTF-8", self.describe())))
    }
}

/// 一项资产在某个源里的**位置**：一个目录（磁盘）或一个同前缀条目组（包）。
///
/// 刻意不把 `sheet`/`polygon` 摊平成字段：那两个名字写在清单里（[`super::Manifest::sheet`]
/// 与 [`super::AccessoryManifest::sheet`]），摊平就是"两处声明同一个成员"。
/// 正确用法 = 先读 [`Self::manifest`] → 解析 → 再 [`Self::member`] 拿清单说的那个名字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetItem {
    pub kind: ItemKind,
    pub id: String,
    /// 谁供的（[`AssetSource::label`]）。错误与 `opet pack list` 都要点名。
    pub source: String,
    loc: ItemLoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ItemLoc {
    /// `<root>/<id>/` 这个目录。
    Dir(PathBuf),
    /// 包内前缀，形如 `pets/demo/`（§3.11.1 的包内布局）。
    Archive { pack: PathBuf, prefix: String },
}

impl AssetItem {
    /// 清单成员（本体 = `pet.json`，配件 = `accessory.json`）。磁盘上的名字是这么定的，
    /// `tools/pack.py` 也这么产 ⇒ **代码为准**，设计书 §3.11.1 那张表里的
    /// `item.json`/`parts.webp` 是旧的（已登记要回写）。
    pub fn manifest(&self) -> Result<AssetHandle, AssetError> {
        self.member(manifest_member(self.kind))
    }

    /// 项目录下的任意成员，名字来自清单 ⇒ **必须走同一套越界判定**。
    pub fn member(&self, rel: &str) -> Result<AssetHandle, AssetError> {
        zip::check_safe_name(rel).map_err(|e| {
            AssetError::new(format!(
                "{} 的 {} 项里成员名非法: {}",
                self.source,
                self.id,
                e.message()
            ))
        })?;
        Ok(match &self.loc {
            ItemLoc::Dir(dir) => AssetHandle::File(dir.join(rel)),
            ItemLoc::Archive { pack, prefix } => AssetHandle::Entry {
                pack: pack.clone(),
                name: format!("{prefix}{rel}"),
            },
        })
    }

    /// 一句话定位，给 `opet pack show` 与启动日志用。
    pub fn describe(&self) -> String {
        match &self.loc {
            ItemLoc::Dir(dir) => format!("{}:{} → {}", self.source, self.id, dir.display()),
            ItemLoc::Archive { pack, prefix } => {
                format!("{}:{} → {}!{prefix}", self.source, self.id, pack.display())
            }
        }
    }
}

/// 每一项 kind 的清单成员名（唯一一份，别在调用方各写一个字面量）。
pub const fn manifest_member(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Pet => "pet.json",
        ItemKind::Accessory => "accessory.json",
    }
}

/// `.petpack` 包内那一类资产的目录名（§3.11.1 的布局）。
pub const fn pack_prefix(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Pet => "pets/",
        ItemKind::Accessory => "accessories/",
    }
}

/// 资产来源。三个实现各自服务什么时机，见 FLOW §7 那张表 ——
/// 不写清就会长出一条"实现了但没人调"的死代码。
pub trait AssetSource {
    /// 报告用：`builtin` / `installed` / 包文件名。
    fn label(&self) -> &str;

    /// 这个源里有没有这一项。`None` = 没有（不是错误；换一个源问就是了）。
    fn locate(&self, kind: ItemKind, id: &str) -> Option<AssetItem>;

    /// 这个源里全部项，**按 `(kind, id)` 升序** —— `read_dir` 与 zip 中央目录的顺序都
    /// 不可依赖（同 `discover_accessories` 里那次排序，理由一字不差）。
    ///
    /// 返回 `Vec<Result<…>>` 而不是 `Vec<AssetItem>`：一个"在这儿但坏了"的项必须能被
    /// 说出口。静默跳过 = 换装页少一件而没人知道为什么（§9.2 第 20 条那个空列表的形状）。
    fn list(&self) -> Vec<Result<AssetItem, AssetError>>;
}

/// 按优先级排好的若干个源：`locate` 取第一个命中的。
///
/// 只给这一个自由函数，不给 `AssetRouter` 类型 —— P1 的三个调用方（`opet pack list/show/verify`、
/// INV 门禁、装包预检）要的只是"在任何一条源里找得到吗"，而运行时的多源优先级到 P2 才与
/// `owned()` 过滤一起接（那时 `discover_accessories` 的签名本来就要改）。
pub fn locate_first(sources: &[&dyn AssetSource], kind: ItemKind, id: &str) -> Option<AssetItem> {
    sources.iter().find_map(|s| s.locate(kind, id))
}

// ---------------------------------------------------------------- 实现一：目录

/// 一个"项目录的集合"：`<root>/<id>/{pet.json|accessory.json, sheet.webp, polygon.json}`。
///
/// `BuiltinDir`（仓库 `assets/`，开发态）与 `PackDir`（`$XDG_DATA_HOME/opet/{pets,accessories}`，
/// 装完之后）**机械上完全一样**，差别只在"这条根是谁、优先级排第几"⇒ 写成一个类型、
/// 由构造方给 `label`。分成两个 struct 就是两条会各自漂移的事实源。
#[derive(Debug, Clone)]
pub struct AssetDir {
    label: String,
    root: PathBuf,
    kind: ItemKind,
}

impl AssetDir {
    /// `root` 是**已经带上 kind 的那一级**（如 `…/opet/accessories`），因为配件与本体
    /// 各自的根由不同的环境变量给（`$OPET_ACCESSORIES` / `$OPET_PETS`），不是同一个 home
    /// 下的两个子目录。
    pub fn new(label: impl Into<String>, root: impl AsRef<Path>, kind: ItemKind) -> Self {
        Self {
            label: label.into(),
            root: root.as_ref().to_path_buf(),
            kind,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn item(&self, id: &str) -> AssetItem {
        AssetItem {
            kind: self.kind,
            id: id.to_string(),
            source: self.label.clone(),
            loc: ItemLoc::Dir(self.root.join(id)),
        }
    }
}

impl AssetSource for AssetDir {
    fn label(&self) -> &str {
        &self.label
    }

    fn locate(&self, kind: ItemKind, id: &str) -> Option<AssetItem> {
        // kind 对不上 ⇒ 没有。这里不报错：一条源只装一类东西是构造时定的事实，
        // 报错的责任在调用方（INV 门禁会说出"这条源是配件根"）。
        if kind != self.kind || zip::check_safe_name(id).is_err() {
            return None;
        }
        let item = self.item(id);
        item.manifest().ok()?.exists().then_some(item)
    }

    fn list(&self) -> Vec<Result<AssetItem, AssetError>> {
        let mut out: Vec<Result<AssetItem, AssetError>> = Vec::new();
        let rd = match std::fs::read_dir(&self.root) {
            Ok(rd) => rd,
            // 第三种情况单独分出来：**这个根还没被创建**。它和"根里没东西"在磁盘侧
            // 是同一个意思（零项），和"读不了"完全不是一回事 —— 不分开的话，一台从没装过包
            // 的机器上 `opet pack list` 每次都冒两条 ⚠，而那两条是它自己的正常状态。
            // ⚠ 只豁免 `NotFound` 这一种：`EACCES` / "路径上有个文件挡着" 照旧出声。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
            // 根读不了 ≠ 根里没东西。这条必须出声：否则"没权限/是文件"
            // 在屏上是同一个空列表。
            Err(e) => {
                return vec![Err(AssetError::new(format!(
                    "{} 的根 {} 读不了: {e}",
                    self.label,
                    self.root.display()
                )))];
            }
        };
        for entry in rd {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                out.push(Err(AssetError::new(format!(
                    "{} 里有个条目名不是合法 UTF-8: {}",
                    self.label,
                    path.display()
                ))));
                continue;
            };
            if name.starts_with('.') {
                // 安装器的暂存目录（`.tmp.<pack>/`）与归属清单（`.installed_by/`）都靠这条
                // 不被当成资产；它们是**点开头**的，不是"我们约定忽略的垃圾"。
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            let item = self.item(name);
            match item.manifest() {
                Ok(h) if h.exists() => out.push(Ok(item)),
                Ok(h) => out.push(Err(AssetError::new(format!(
                    "{} 里 {name} 没有清单 {}",
                    self.label,
                    h.describe()
                )))),
                Err(e) => out.push(Err(e)),
            }
        }
        out.sort_by(|a, b| match (a, b) {
            (Ok(x), Ok(y)) => sort_key(x).cmp(&sort_key(y)),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(x), Err(y)) => x.0.cmp(&y.0),
        });
        out
    }
}

// ---------------------------------------------------------------- 实现二：包

/// 一个**未拆**的 `.petpack`（store-only zip）。服务"装之前"那一段：商店预览、
/// 完整性检查、INV 预检 ⇒ 它不需要、也不该把任何东西写到磁盘上。
#[derive(Debug, Clone)]
pub struct AssetArchive {
    label: String,
    zip: ZipIndex,
}

impl AssetArchive {
    /// 打开一个包。**只有 `ZipIndex::open` 全过才建得出这个源**：越界条目名、symlink、
    /// deflate 在打开这一刻就出局（§3.11.1 那条"整个包拒绝"）。
    pub fn open(path: &Path) -> Result<Self, zip::ZipError> {
        let zip = ZipIndex::open(path)?;
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<pack>")
            .to_string();
        Ok(Self { label, zip })
    }

    pub fn entries(&self) -> &[zip::ZipEntry] {
        self.zip.entries()
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, zip::ZipError> {
        self.zip.read(name)
    }

    fn item(&self, kind: ItemKind, id: &str) -> AssetItem {
        AssetItem {
            kind,
            id: id.to_string(),
            source: self.label.clone(),
            loc: ItemLoc::Archive {
                pack: self.zip.file().to_path_buf(),
                prefix: format!("{}{id}/", pack_prefix(kind)),
            },
        }
    }

    /// 包内一条清单条目名 → `(kind, id)`。布局按 §3.11.1：`pets/<slug>/pet.json`、
    /// `accessories/<slug>/accessory.json`。别的深度（`pets/x/y/pet.json`）不是我们的布局，
    /// 当"没看见"处理而不是当错误 —— 它读不出一份资产，INV 与 `verify` 会报缺什么。
    fn classify(&self, name: &str) -> Option<(ItemKind, String)> {
        for kind in ItemKind::ALL {
            let tail = format!("/{}", manifest_member(kind));
            if let Some(rest) = name.strip_prefix(pack_prefix(kind))
                && let Some(id) = rest.strip_suffix(tail.as_str())
                && !id.is_empty()
                && !id.contains('/')
            {
                return Some((kind, id.to_string()));
            }
        }
        None
    }
}

impl AssetSource for AssetArchive {
    fn label(&self) -> &str {
        &self.label
    }

    fn locate(&self, kind: ItemKind, id: &str) -> Option<AssetItem> {
        if zip::check_safe_name(id).is_err() {
            return None;
        }
        let member = format!("{}{id}/{}", pack_prefix(kind), manifest_member(kind));
        self.zip.find(&member).map(|_| self.item(kind, id))
    }

    fn list(&self) -> Vec<Result<AssetItem, AssetError>> {
        let mut items: Vec<AssetItem> = self
            .zip
            .entries()
            .iter()
            .filter_map(|e| {
                self.classify(&e.name)
                    .map(|(kind, id)| self.item(kind, &id))
            })
            .collect();
        items.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        // 同一个 id 在同一个前缀下只可能有一条真清单；出现两条是包坏了（写侧不去重，
        // 因为"去重还是报错"属于装包门禁的判断，不属于源）。这里取稳定的一条。
        items.dedup_by(|a, b| sort_key(a) == sort_key(b));
        items.into_iter().map(Ok).collect()
    }
}

/// 排序键。**不用 `ItemKind` 的 `Ord`（它没有，也不该有）**：稀有度/类型的"谁在前"属于
/// UI 与目录，不属于源；这里要的只是"同一次运行两次列表一样"。
fn sort_key(item: &AssetItem) -> (&'static str, &str) {
    (item.kind.as_str(), item.id.as_str())
}

/// 开发态那条源（仓库 `assets/`）与装完包那条源（`$XDG_DATA_HOME/opet/…`）的便捷构造。
/// 两个名字而不是一个通用名，是因为**优先级是它们的区别**（内置最低，见 FLOW §7 表）；
/// 类型上它们是同一个 [`AssetDir`]。
pub fn builtin_dir(root: impl AsRef<Path>, kind: ItemKind) -> AssetDir {
    AssetDir::new("builtin", root, kind)
}

pub fn installed_dir(root: impl AsRef<Path>, kind: ItemKind) -> AssetDir {
    AssetDir::new("installed", root, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::zip::write_store_zip;

    fn scratch(test: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("opet_src_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("临时目录建得起来");
        dir
    }

    /// 一份**能过 `Manifest::validate`** 的最小本体清单（单 `idle` 档，M2 首批产物的形状）。
    fn pet_json(slug: &str) -> String {
        format!(
            r#"{{
              "format": 2, "slug": "{slug}", "display_name": "测试",
              "sheet": "sheet.webp",
              "grid": {{ "cols": 8, "rows": 4, "w": 192, "h": 192 }},
              "states": {{ "idle": {{"rows": [0, 1, 2, 3], "fps": 8, "loop": true}} }},
              "anchor": {{ "feet": [96, 180] }}
            }}"#
        )
    }

    /// `fits` 故意不写：写了就得让夹具 slug 对上，而这几条测试要比的是"同寻址放行"，
    /// 不是 fits（fits 那一支在 `accessory.rs` 的测试里已经单独钉过）。
    fn accessory_json(slug: &str) -> String {
        format!(
            r#"{{
              "format": 2, "slug": "{slug}", "slot": "head",
              "sheet": "sheet.webp",
              "grid": {{ "cols": 8, "rows": 4, "w": 192, "h": 192 }},
              "states": {{ "idle": {{"rows": [0, 1, 2, 3], "fps": 8, "loop": true}} }}
            }}"#
        )
    }

    /// 在 `root/<id>/` 下摆一项完整资产：清单 + `sheet.webp` + `polygon.json`。
    fn seed_dir(root: &Path, id: &str, manifest: &str) {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(manifest), manifest_bytes(id)).unwrap();
        std::fs::write(d.join("sheet.webp"), format!("SHEET-{id}")).unwrap();
        std::fs::write(d.join("polygon.json"), "{}").unwrap();
    }

    fn manifest_bytes(id: &str) -> Vec<u8> {
        if id.starts_with("hat") {
            accessory_json(id).into_bytes()
        } else {
            pet_json(id).into_bytes()
        }
    }

    fn ids(items: &[Result<AssetItem, AssetError>]) -> Vec<String> {
        items
            .iter()
            .map(|r| match r {
                Ok(i) => format!("{}/{}/ok", i.kind.as_str(), i.id),
                Err(e) => format!("ERR/{}", e.message()),
            })
            .collect()
    }

    #[test]
    fn a_directory_source_finds_an_item_and_reads_bytes_through_its_manifest() {
        let dir = scratch("dir_happy");
        let pets = dir.join("pets");
        std::fs::create_dir_all(&pets).unwrap();
        seed_dir(&pets, "demo", "pet.json");

        let src = installed_dir(&pets, ItemKind::Pet);
        assert_eq!(src.label(), "installed");
        let item = src
            .locate(ItemKind::Pet, "demo")
            .expect("装好的一项必须定位得到");
        assert_eq!(item.kind, ItemKind::Pet);

        // **这条序列就是 §3.11.1 说的"内置与包内走同一条代码路径"**：
        // 清单 → 解析 → 按清单里写的 sheet 名取字节 → 内核寻址。
        let text = item.manifest().unwrap().read_text().unwrap();
        let m = crate::assets::Manifest::parse(&text).expect("清单要能被内核解析");
        assert_eq!(m.slug, "demo");
        let sheet = item.member(&m.sheet).unwrap();
        assert_eq!(sheet.read().unwrap(), b"SHEET-demo");
        assert!(item.member("polygon.json").unwrap().exists());
        assert_eq!(src.locate(ItemKind::Pet, "没有这只"), None);
        assert_eq!(
            src.locate(ItemKind::Accessory, "demo"),
            None,
            "这条根只装本体"
        );
    }

    /// 越界的成员名（写在清单里的 `sheet` 字段，来自别人打的包）必须出局。
    /// 这是本模块唯一一条**外部输入直接参与路径拼接**的通道，也是它唯一的路径穿越面。
    #[test]
    fn a_traversal_member_name_from_a_manifest_is_rejected_not_resolved() {
        let dir = scratch("dir_traversal");
        let pets = dir.join("pets");
        std::fs::create_dir_all(&pets).unwrap();
        seed_dir(&pets, "demo", "pet.json");
        let item = installed_dir(&pets, ItemKind::Pet)
            .locate(ItemKind::Pet, "demo")
            .unwrap();
        for bad in ["../../etc/passwd", "/etc/passwd", "a//b", ""] {
            let err = item
                .member(bad)
                .expect_err("越界成员名 {bad} 不该拼出一个句柄");
            let msg = err.message();
            assert!(msg.contains("installed"), "要点名是哪条源: {msg}");
            assert!(msg.contains("demo"), "要点名是哪一项: {msg}");
        }
        // 反斜杠不是分隔符（zip 用 `/`），所以这只是一个怪名字，不是穿越 —— 允许。
        assert!(item.member("we..b\\ack").is_ok());
    }

    /// "在这儿但坏了"必须被说出口：`list` 的一条 `Err`，而不是从列表里消失。
    /// 这一条直接对着 §9.2 第 20 条那个"没有任何解释的空列表"。
    #[test]
    fn a_directory_without_a_manifest_is_reported_not_skipped() {
        let dir = scratch("dir_broken");
        let acc = dir.join("accessories");
        std::fs::create_dir_all(acc.join("hat_ok")).unwrap();
        std::fs::write(acc.join("hat_ok/accessory.json"), accessory_json("hat_ok")).unwrap();
        std::fs::create_dir_all(acc.join("hat_broken")).unwrap(); // 空目录：装了一半
        std::fs::write(acc.join("loose.txt"), b"x").unwrap(); // 不是目录
        let src = installed_dir(&acc, ItemKind::Accessory);
        let got = ids(&src.list());
        assert_eq!(
            got,
            vec![
                "accessory/hat_ok/ok".to_string(),
                format!(
                    "ERR/installed 里 hat_broken 没有清单 {}",
                    acc.join("hat_broken/accessory.json").display()
                )
            ],
            "坏的那条要排在好之后，并点名缺哪个文件"
        );
    }

    /// 安装器的暂存与归属清单都在根目录下，都靠"点开头"不被当成资产。
    /// 这条测试钉的是**约定本身**：哪天有人把 `.installed_by` 改名成 `installed_by`，
    /// 它会红，而那正是"把归属清单当配件列出来"的起点。
    #[test]
    fn installer_bookkeeping_directories_are_not_assets() {
        let dir = scratch("dir_dot");
        let pets = dir.join("pets");
        std::fs::create_dir_all(&pets).unwrap();
        seed_dir(&pets, "demo", "pet.json");
        seed_dir(&pets, ".tmp_cat_series_01", "pet.json");
        std::fs::create_dir_all(pets.join(".installed_by/cat_series_01")).unwrap();
        std::fs::write(pets.join(".installed_by/cat_series_01/list"), b"demo\n").unwrap();
        let got = ids(&installed_dir(&pets, ItemKind::Pet).list());
        assert_eq!(got, vec!["pet/demo/ok"]);
    }

    /// 根读不了 ≠ 根里没东西，两者在屏上必须是两句话。
    #[test]
    fn an_unreadable_root_says_so_instead_of_looking_empty() {
        let dir = scratch("dir_noroot");
        let a_file = dir.join("notadir");
        std::fs::write(&a_file, b"x").unwrap();
        let src = builtin_dir(&a_file, ItemKind::Pet);
        let got = src.list();
        assert_eq!(got.len(), 1, "一条错误，不是空列表");
        let msg = got[0].as_ref().err().unwrap().message();
        assert!(msg.contains("builtin"), "{msg}");
        assert!(msg.contains("读不了"), "{msg}");
        assert!(
            src.locate(ItemKind::Pet, "demo").is_none(),
            "读不了就是没有"
        );
    }

    /// **不存在**是第三种情况：它等于"盘上零项"，不是"读不了"。豁免只开这一种 ——
    /// `EACCES` 那类上面那条测试已经钉住必须出声。
    #[test]
    fn a_root_that_was_never_created_lists_empty_instead_of_complaining() {
        let dir = scratch("dir_absent");
        let src = builtin_dir(dir.join("pets"), ItemKind::Pet);
        assert!(
            src.list().is_empty(),
            "没装过包的机器不该每条命令都冒 ⚠: {:?}",
            src.list()
        );
    }

    /// 排序 = 确定性。同一次运行两次 `list` 必须逐位一样（`read_dir` 不保证顺序）。
    #[test]
    fn directory_listing_is_sorted_and_stable() {
        let dir = scratch("dir_sorted");
        let pets = dir.join("pets");
        std::fs::create_dir_all(&pets).unwrap();
        for id in ["zulu", "alpha", "mike"] {
            seed_dir(&pets, id, "pet.json");
        }
        let src = installed_dir(&pets, ItemKind::Pet);
        let once: Vec<String> = src
            .list()
            .iter()
            .map(|i| i.as_ref().unwrap().id.clone())
            .collect();
        assert_eq!(once, ["alpha", "mike", "zulu"]);
        assert_eq!(
            once,
            src.list()
                .iter()
                .map(|i| i.as_ref().unwrap().id.clone())
                .collect::<Vec<_>>()
        );
    }

    /// **跨实现的那条**：读真 `zip -0` 打的包，把它当成一个资产源。
    /// 夹具里有 `pets/demo/pet.json` 与 `accessories/hat/accessory.json`（见 zip.rs 的同名测试）。
    #[test]
    fn a_real_pack_is_a_source_with_both_kinds() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/zip_sample_stored.zip");
        let src = AssetArchive::open(&fixture).expect("真包要能当源开");
        assert_eq!(src.label(), "zip_sample_stored.zip");
        let got = ids(&src.list());
        assert_eq!(
            got,
            ["accessory/hat/ok".to_string(), "pet/demo/ok".to_string()],
            "accessory < pet（按 kind 名排），且只认 <kind前缀>/<id>/<清单名> 这一层"
        );
        // 取一份清单的字节，看它真的来自包而不是磁盘
        let pet = src.locate(ItemKind::Pet, "demo").expect("包里有 demo");
        let text = pet.manifest().unwrap().read_text().unwrap();
        assert!(text.contains("demo"), "{text}");
        assert!(pet.describe().contains("!pets/demo/"), "{}", pet.describe());
        // 包里没有的成员 ⇒ 读的时候点名包，不静默给空字节
        let err = pet
            .member("sheet.webp")
            .unwrap()
            .read()
            .expect_err("夹具没有 sheet");
        assert!(err.message().contains("zip_sample_stored.zip"), "{err}");
    }

    /// 自造的**完整**包：走"清单 → 按 sheet 名取字节"那条装载序列，与磁盘那侧同一条代码。
    #[test]
    fn a_full_pack_round_trips_through_the_same_path_as_a_directory() {
        let dir = scratch("pack_full");
        let pack = dir.join("cat_series_01.petpack");
        // `evil` 那份的 `sheet` 字段是越界的：包能开、项能列，**取字节那一刻**必须出局。
        // 这是第三方包唯一能影响我们路径的通道（清单里的成员名是不可信输入）。
        let evil = pet_json("evil").replace("\"sheet.webp\"", "\"../../etc/passwd\"");
        let bytes = write_store_zip(&[
            ("manifest.toml", b"id = \"cat_series_01\"\n"),
            ("pets/demo/pet.json", pet_json("demo").as_bytes()),
            ("pets/demo/sheet.webp", b"SHEET-PET"),
            ("pets/demo/polygon.json", b"{}"),
            ("pets/evil/pet.json", evil.as_bytes()),
            ("pets/evil/sheet.webp", b"NEVER-READ"),
            (
                "accessories/hat/accessory.json",
                accessory_json("hat").as_bytes(),
            ),
            ("accessories/hat/sheet.webp", b"SHEET-ACC"),
            ("pets/deep/nested/pet.json", b"{}"),
        ])
        .unwrap();
        std::fs::write(&pack, bytes).unwrap();
        let src = AssetArchive::open(&pack).unwrap();
        let got = ids(&src.list());
        assert_eq!(
            got,
            [
                "accessory/hat/ok".to_string(),
                "pet/demo/ok".to_string(),
                "pet/evil/ok".to_string()
            ],
            "深层的 pets/deep/nested/pet.json 不是我们的布局 ⇒ 不列（INV 会报缺）"
        );

        let pet = src.locate(ItemKind::Pet, "demo").unwrap();
        let m =
            crate::assets::Manifest::parse(&pet.manifest().unwrap().read_text().unwrap()).unwrap();
        assert_eq!(pet.member(&m.sheet).unwrap().read().unwrap(), b"SHEET-PET");
        let acc = src.locate(ItemKind::Accessory, "hat").unwrap();
        let a =
            crate::assets::AccessoryManifest::parse(&acc.manifest().unwrap().read_text().unwrap())
                .unwrap();
        assert_eq!(acc.member(&a.sheet).unwrap().read().unwrap(), b"SHEET-ACC");
        a.validate_against(&m).expect("同寻址应放行");

        let evil_item = src.locate(ItemKind::Pet, "evil").unwrap();
        let em =
            crate::assets::Manifest::parse(&evil_item.manifest().unwrap().read_text().unwrap())
                .expect("越界成员名不影响清单自身合法");
        let err = evil_item
            .member(&em.sheet)
            .expect_err("清单里写 ../../ 必须出局，读不到 NEVER-READ 那份字节");
        let msg = err.message();
        assert!(msg.contains("evil"), "要点名是哪一项: {msg}");
        assert!(
            msg.contains("cat_series_01.petpack"),
            "要点名是哪个包: {msg}"
        );

        assert!(
            src.locate(ItemKind::Pet, "deep").is_none(),
            "深层嵌套不算一项资产"
        );
        assert!(
            src.locate(ItemKind::Pet, "../evil").is_none(),
            "越界 id 不进拼接"
        );
    }

    /// id 与句柄的关系：`Entry` 的 `exists()` 查的是**包文件**还在不在，不是条目。
    /// 这条语义要钉住，否则"包被删了但索引还在内存里"会表现成"资产存在"。
    #[test]
    fn a_pack_handle_reports_the_pack_file_not_the_entry() {
        let handle = AssetHandle::Entry {
            pack: PathBuf::from("/does/not/exist.petpack"),
            name: "pets/demo/pet.json".into(),
        };
        assert!(!handle.exists());
        assert!(
            handle.describe().ends_with("!pets/demo/pet.json"),
            "{}",
            handle.describe()
        );
        assert!(handle.read().is_err());
        let dir = scratch("handle_file");
        let absent = AssetHandle::File(dir.join("nope"));
        assert!(!absent.exists());
        assert!(absent.read().unwrap_err().message().starts_with("读 "));
    }

    /// 多源优先级：**第一个命中的源赢**，而 `source` 字段说得出是谁。
    /// 同 id 两份内容 ⇒ 不合并、不报错，只按顺序取（覆盖安装的事归安装器，不归源）。
    #[test]
    fn locate_first_takes_the_highest_priority_source_and_says_who() {
        let dir = scratch("router");
        let installed = dir.join("installed");
        let builtin = dir.join("builtin");
        for root in [&installed, &builtin] {
            std::fs::create_dir_all(root).unwrap();
            seed_dir(root, "demo", "pet.json");
        }
        std::fs::write(installed.join("demo/sheet.webp"), b"SHEET-INSTALLED").unwrap();
        std::fs::write(builtin.join("demo/sheet.webp"), b"SHEET-BUILTIN").unwrap();

        let hi = installed_dir(&installed, ItemKind::Pet);
        let lo = builtin_dir(&builtin, ItemKind::Pet);
        let sources: &[&dyn AssetSource] = &[&hi, &lo];
        let item = locate_first(sources, ItemKind::Pet, "demo").expect("两处都有 ⇒ 一定命中");
        assert_eq!(item.source, "installed");
        assert_eq!(
            item.member("sheet.webp").unwrap().read().unwrap(),
            b"SHEET-INSTALLED"
        );
        // 顺序反过来，字节跟着变 —— 证明赢的是**顺序**，不是别的
        let flipped: &[&dyn AssetSource] = &[&lo, &hi];
        let item = locate_first(flipped, ItemKind::Pet, "demo").unwrap();
        assert_eq!(item.source, "builtin");
        assert_eq!(
            item.member("sheet.webp").unwrap().read().unwrap(),
            b"SHEET-BUILTIN"
        );
        let empty: &[&dyn AssetSource] = &[];
        assert!(
            locate_first(empty, ItemKind::Pet, "demo").is_none(),
            "没有源就是没有"
        );
    }

    /// `manifest_member` / `pack_prefix` 是**唯一一份**这类名字的定义；这条把它们和
    /// 磁盘与 §3.11.1 的实际布局钉在一起（写错一个字母，上面所有测试一起红，但那不够 ——
    /// 这里点名"是哪两个常量"）。
    #[test]
    fn the_member_and_prefix_constants_are_the_single_definition() {
        assert_eq!(manifest_member(ItemKind::Pet), "pet.json");
        assert_eq!(manifest_member(ItemKind::Accessory), "accessory.json");
        assert_eq!(pack_prefix(ItemKind::Pet), "pets/");
        assert_eq!(pack_prefix(ItemKind::Accessory), "accessories/");
    }
}
