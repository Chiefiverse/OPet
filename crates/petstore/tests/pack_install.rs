//! P1 的验收判据（FLOW §9 那一行）落到文件系统的形状：
//!
//! - **装完能在资产根里看到、且 `inventory` 里有对应行**（`opening_a_clean_pack…`、
//!   `a_clean_free_pack_installs…`、`a_bundle_reaches_both_roots…`）；
//! - **半途失败的坏包一个字都不留**（`a_rejected_pack…`、`an_oversized…`、
//!   `a_paid_pack_item_that_is_in_the_gacha_pool…`（含"免费包同一份内容要能装"的反向证明）、
//!   `verify_reports…` 四条，每条都回查"资产根是空的、库里没有行"）；
//! - **`verify` 不解压就能报出包内缺哪件**（`verify_reports_which_item_is_unloadable`）；
//! - **卸载只删自己解出来的东西**（`ownership_list_gates_uninstall`，手工塞的文件必须活着）。
//!
//! 夹具全部**自造**（`write_store_zip`）：不依赖 CI 机器上有没有 `/usr/bin/zip`，那条依赖
//! 会让测试在有的机器上静默跳过 = 假门禁。资产根走 [`Env`] 注入（`$OPET_PETS` /
//! `$OPET_ACCESSORIES` ⇒ `AssetRootSource::EnvVar`），所以这些测试**从不碰**用户真实的
//! `$XDG_DATA_HOME`，也不依赖 `$HOME` 存不存在。

use std::path::{Path, PathBuf};

use petcore::assets::zip::write_store_zip;
use petcore::gacha::{Catalog, ItemKind};
use petstore::Store;
use petstore::licenses::ContentPack;
use petstore::pack::{self, InstallOpts, MAX_PACK_MB};
use petstore::paths::Env;

/// 一件配件的最小可解析清单（`sheet` 故意叫 `sheet.webp`，与夹具里那个成员名一致）。
const ACC_JSON: &str = r#"{"format":1,"slug":"pack_hat_red","slot":"head","sheet":"sheet.webp",
"grid":{"cols":8,"rows":1,"w":96,"h":96},"states":{"idle":{"rows":[0],"fps":5.0}}}"#;

/// 一只本体的最小可解析清单。
const PET_JSON: &str = r#"{"format":1,"slug":"pack_demo_cat","sheet":"sheet.webp",
"grid":{"cols":8,"rows":1,"w":96,"h":96},"states":{"idle":{"rows":[0],"fps":5.0}},
"anchor":{"feet":[48.0,90.0]}}"#;

const SHEET: &[u8] = b"RIFFxxxxWEBPVP8 ";

fn toml(id: &str, kind: &str, items: &[&str], store: &str, price: Option<i64>) -> String {
    let list = items
        .iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let price = match price {
        Some(c) => format!("price_cents = {c}\ncurrency_code = \"USD\"\n"),
        None => String::new(),
    };
    format!(
        "format = 1\nid = \"{id}\"\ntitle = \"演示包\"\nkind = \"{kind}\"\n{price}store = \"{store}\"\n\
         pack_path = \"发布机上写的相对路径.toml\"\nitems = [{list}]\n"
    )
}

/// 一份自造的包 + 两个临时资产根 + 一个内存库。每个测试自己造一个，互不共享状态。
struct Harness {
    tmp: tempfile::TempDir,
    acc: PathBuf,
    pets: PathBuf,
    env: Env,
}

impl Harness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let acc = tmp.path().join("accessories");
        let pets = tmp.path().join("pets");
        std::fs::create_dir_all(&acc).unwrap();
        std::fs::create_dir_all(&pets).unwrap();
        let env = Env {
            opet_accessories: Some(acc.display().to_string()),
            opet_pets: Some(pets.display().to_string()),
            // 故意不给 `home` / `xdg_data_home`：两级回退在临时根之下，注入的三条里
            // 只要前两级是空的，测试就可能写到真实 `$HOME` 上。
            home: None,
            xdg_data_home: None,
            opet_db: None,
        };
        Self {
            tmp,
            acc,
            pets,
            env,
        }
    }

    /// 把条目写成 `.petpack`（store-only zip）并返回它的路径。包文件本身也留在临时目录里。
    fn zip(&self, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let bytes = write_store_zip(entries).expect("夹具要能写成 zip");
        let path = self.tmp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// 干净包的那份 `manifest.toml` 文本（测试要拿它算字节数，所以不能只留在 `clean_pack` 里）。
    fn clean_manifest(&self) -> String {
        toml("demo_01", "accessories", &["pack_hat_red"], "free", None)
    }

    /// 一个**干净的单配件包**（`kind = accessories`、`store = free`、一件 `pack_hat_red`）。
    fn clean_pack(&self) -> PathBuf {
        self.zip(
            "clean.petpack",
            &[
                (pack::MANIFEST_MEMBER, self.clean_manifest().as_bytes()),
                (
                    "accessories/pack_hat_red/accessory.json",
                    ACC_JSON.as_bytes(),
                ),
                ("accessories/pack_hat_red/sheet.webp", SHEET),
            ],
        )
    }

    /// 资产根下的**顶层**条目名（升序）。"一个字都不留"就是拿它判的。
    fn top(&self, root: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// 某个目录下的文件（递归，相对该目录，升序）。
    fn tree(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut walk = vec![dir.to_path_buf()];
        while let Some(p) = walk.pop() {
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk.push(path);
                } else if let Ok(rel) = path.strip_prefix(dir) {
                    out.push(rel.display().to_string());
                }
            }
        }
        out.sort();
        out
    }
}

/// 只含一件配件的干净 catalog（用于"这一件在不在随机池里"的判定）。
fn catalog_with(ids: &[&str]) -> Catalog {
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                r#"{{"id":"{id}","name":"{id}","kind":"accessory","rarity":"N","weight":1000,"slot":"head"}}"#
            )
        })
        .collect();
    let json = format!(
        r#"{{"format":1,"id":"t","banner_id":"t","items":[{}]}}"#,
        items.join(",")
    );
    Catalog::from_json(&json).expect("夹具 catalog 要能解析")
}

fn empty_catalog() -> Catalog {
    Catalog::from_json(r#"{"format":1,"id":"t","banner_id":"t","items":[]}"#).unwrap()
}

#[test]
fn opening_a_clean_pack_names_every_entry_it_holds() {
    let h = Harness::new();
    let manifest = h.clean_manifest();
    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    assert_eq!(pack.manifest.id, "demo_01");
    assert_eq!(
        pack.items,
        vec![(ItemKind::Accessory, "pack_hat_red".into())]
    );
    // `pack_path` 以调用方给的路径为准，清单里那份（发布机的相对路径）不采信。
    assert_eq!(pack.manifest.pack_path, path.display().to_string());
    assert_eq!(pack.entry_count, 3, "manifest + 清单 + sheet");
    // 体积门禁判的是**解出来的字节总和**，不是 zip 文件本身的大小（zip bomb 的经典漏法）。
    assert_eq!(
        pack.bytes as usize,
        manifest.len() + ACC_JSON.len() + SHEET.len()
    );
}

#[test]
fn a_clean_free_pack_installs_into_the_asset_root_and_the_ledger() {
    let h = Harness::new();
    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let out = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();

    assert_eq!(out.pack_id, "demo_01");
    assert_eq!(out.files, 2, "accessory.json + sheet.webp");
    assert_eq!(out.grant.added, vec!["accessory/pack_hat_red".to_string()]);
    assert!(out.grant.already.is_empty());
    assert!(out.unsigned, "夹具没写 signature ⇒ 未验证要能报出来");
    assert!(out.source.starts_with("free"), "{}", out.source);
    assert!(out.took_over.is_empty());
    assert_eq!(out.roots.len(), 1);

    // 资产根里真的有那两个文件（验收判据的前半句）。
    assert_eq!(
        h.tree(&h.acc.join("pack_hat_red")),
        vec!["accessory.json".to_string(), "sheet.webp".to_string()]
    );
    // 暂存目录不留：装完之后根里**只有**这两样 ⇒ 任何 `.tmp.<id>` 空壳都逃不过这条。
    assert_eq!(
        h.top(&h.acc),
        vec![pack::OWNER_DIR.to_string(), "pack_hat_red".to_string()],
        "归属清单目录在前（`.` < `p`），别把两样的顺序记成偶然"
    );
    // 归属清单 = 相对资产根、升序、一行一个。
    let list = std::fs::read_to_string(h.acc.join(pack::OWNER_DIR).join("demo_01")).unwrap();
    assert_eq!(
        list, "pack_hat_red/accessory.json\npack_hat_red/sheet.webp\n",
        "归属清单的形状直接决定卸载能删什么"
    );
    // 库存里有对应行，且走的是 `pack:` 那条所有权路径（验收判据的后半句）。
    assert_eq!(
        store.pack_items("demo_01").unwrap(),
        vec![(ItemKind::Accessory, "pack_hat_red".into())]
    );
    assert!(store.owned().unwrap().contains("pack_hat_red"));
    assert_eq!(store.item_count("pack_hat_red").unwrap(), 1);
    // 注册表里那一行也在（`opet pack list` 的左半边）。
    let reg = pack::registered(&store).unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].id, "demo_01");
    assert_eq!(reg[0].inventory_rows, 1);
}

#[test]
fn a_rejected_pack_leaves_not_a_single_file_and_not_a_single_row() {
    let h = Harness::new();
    // 声明了两件，包里只有一份清单 ⇒ `Pack::open` 的一致性门禁就该拒绝。
    let manifest = toml(
        "demo_02",
        "accessories",
        &["pack_hat_red", "pack_hat_blue"],
        "free",
        None,
    );
    let path = h.zip(
        "half.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            (
                "accessories/pack_hat_red/accessory.json",
                ACC_JSON.as_bytes(),
            ),
        ],
    );
    let err = pack::Pack::open(&path, MAX_PACK_MB).unwrap_err();
    assert!(err.contains("pack_hat_blue"), "要点名缺的那件: {err}");

    let store = Store::in_memory().unwrap();
    assert!(store.owned().unwrap().is_empty());
    assert!(pack::registered(&store).unwrap().is_empty());
    // 资产根顶层：一个点开头的暂存、一个 `.installed_by` 都不该有。
    assert_eq!(h.top(&h.acc), Vec::<String>::new());
    assert_eq!(h.top(&h.pets), Vec::<String>::new());
}

#[test]
fn an_oversized_pack_is_refused_at_open_not_at_extraction() {
    let h = Harness::new();
    let path = h.clean_pack();
    // `max_mb` 只许调**大**：调到 0 是为了在这台机器上造出一条"超限"，而不是给用户留一个
    // "关掉体积门禁"的开关（CLI 侧 `--max-mb` 小于默认值会被拒）。
    let err = pack::Pack::open(&path, 0).unwrap_err();
    assert!(err.contains("超过上限"), "{err}");
    assert!(pack::Pack::open(&path, MAX_PACK_MB).is_ok());
    assert_eq!(MAX_PACK_MB, 64, "§3.11.1 的 64 MiB");
    // 拒绝发生在开包 ⇒ 资产根与库都没被碰过（这里连 `install` 都没跑到）。
    assert_eq!(h.top(&h.acc), Vec::<String>::new());
}

#[test]
fn a_paid_pack_item_that_is_in_the_gacha_pool_is_refused_before_any_write() {
    // §3.11.5 的主语是**付费包**：收钱的东西不许同时是抽得到的奖（"付费概率"观感）。
    // 免费包带池内件是另一回事 ⇒ 见 `a_free_pack_carrying_pool_items_lands_pixels_only`。
    let h = Harness::new();
    let manifest = toml(
        "demo_01",
        "accessories",
        &["pack_hat_red"],
        "itch",
        Some(990),
    );
    let path = h.zip(
        "paid_pool.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            (
                "accessories/pack_hat_red/accessory.json",
                ACC_JSON.as_bytes(),
            ),
            ("accessories/pack_hat_red/sheet.webp", SHEET),
        ],
    );
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let err = pack::install(
        &pack,
        &h.env,
        &mut store,
        &catalog_with(&["pack_hat_red"]),
        &InstallOpts::default(),
    )
    .unwrap_err();
    assert!(err.contains("§3.11.5"), "{err}");
    assert_eq!(
        h.top(&h.acc),
        Vec::<String>::new(),
        "撞池的付费包不该解出一个文件"
    );
    assert!(store.owned().unwrap().is_empty());
    assert!(pack::registered(&store).unwrap().is_empty());

    // 反向证明拒绝来自"在池里 × 收了钱"这两个条件，而不是这个 id 有别的毛病：
    // ① 同一份内容放进**空池** ⇒ 过（说明内容本身没问题）；
    let ok = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    );
    assert!(ok.is_ok(), "{ok:?}");
    // ② 同一个池、同样内容，但改成**免费包** ⇒ 也过（拒绝的从来不是"撞池"这一半）。
    let free = pack::Pack::open(&h.clean_pack(), MAX_PACK_MB).unwrap();
    let ok = pack::install(
        &free,
        &h.env,
        &mut store,
        &catalog_with(&["pack_hat_red"]),
        &InstallOpts::default(),
    );
    assert!(
        ok.is_ok(),
        "免费包带池内件要能装（INV 靠它才绿得起来）: {ok:?}"
    );
}

#[test]
fn a_free_pack_carrying_pool_items_lands_pixels_but_grants_no_ownership() {
    // 用户 11-05 拍板的那条分界：**落像素**与**给所有权**是两件事。
    // 池内件的像素必须能随官方免费包分发（否则 `catalog validate --inv` 永远红），
    // 但给它写 inventory 行 = 一条命令送完整池 ⇒ J1 直接失效。
    let h = Harness::new();
    let pack = pack::Pack::open(&h.clean_pack(), MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let out = pack::install(
        &pack,
        &h.env,
        &mut store,
        &catalog_with(&["pack_hat_red"]),
        &InstallOpts::default(),
    )
    .unwrap();
    // 像素落盘了（文件与归属清单都在，卸载才删得掉）。
    assert_eq!(out.files, 2, "清单 + sheet 都要落");
    assert_eq!(
        h.top(&h.acc),
        vec![pack::OWNER_DIR.to_string(), "pack_hat_red".to_string()]
    );
    // 而所有权一行都没写。
    assert_eq!(
        out.pixels_only,
        vec!["accessory/pack_hat_red".to_string()],
        "只落像素的那一批必须点名"
    );
    assert!(out.grant.added.is_empty(), "{:?}", out.grant);
    assert!(
        store.owned().unwrap().is_empty(),
        "池内件不许由包给出所有权"
    );
    // 包本身照记（`content_packs` + 权益）：分发过这件事是真的，只是它不给奖。
    assert_eq!(pack::registered(&store).unwrap().len(), 1);
}

#[test]
fn verify_reports_which_item_is_unloadable_without_touching_disk() {
    let h = Harness::new();
    // 清单说 sheet 叫 `parts.webp`，包里那个成员实际叫 `sheet.webp` ⇒ "包内缺这一件"的形状。
    let manifest = toml("demo_03", "accessories", &["pack_hat_red"], "free", None);
    let bad_json = ACC_JSON.replace("\"sheet.webp\"", "\"parts.webp\"");
    let path = h.zip(
        "wrongsheet.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            (
                "accessories/pack_hat_red/accessory.json",
                bad_json.as_bytes(),
            ),
            ("accessories/pack_hat_red/sheet.webp", SHEET),
        ],
    );
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let check = pack.check(&empty_catalog());
    assert_eq!(check.bad_items(), 1);
    assert_eq!(check.items[0].id, "pack_hat_red");
    assert!(
        check.items[0].problems[0].contains("parts.webp"),
        "要点名缺的那个成员名: {:?}",
        check.items[0].problems
    );
    assert_eq!(check.summary(), "items=1 bad=1 in_pool=0 forbidden=0");

    let mut store = Store::in_memory().unwrap();
    let err = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap_err();
    assert!(err.contains("解析不了"), "{err}");
    assert_eq!(h.top(&h.acc), Vec::<String>::new());
    assert!(store.owned().unwrap().is_empty());
}

#[test]
fn a_manifest_that_does_not_parse_is_named_not_swallowed() {
    let h = Harness::new();
    // `weight` 是红线 1 的键；TOML 侧的未知键一律点名拒绝（与 JSON 侧同一套敌意键）。
    let mut manifest = toml("demo_04", "accessories", &["pack_hat_red"], "free", None);
    manifest.push_str("weight = 0.02\n");
    let path = h.zip(
        "weighted.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            (
                "accessories/pack_hat_red/accessory.json",
                ACC_JSON.as_bytes(),
            ),
            ("accessories/pack_hat_red/sheet.webp", SHEET),
        ],
    );
    let err = pack::Pack::open(&path, MAX_PACK_MB).unwrap_err();
    assert!(err.contains("未知字段") && err.contains("weight"), "{err}");
    assert_eq!(h.top(&h.acc), Vec::<String>::new());
}

#[test]
fn overwriting_a_foreign_directory_needs_force_and_says_so() {
    let h = Harness::new();
    // 手工塞一份同名目录（开发态那条形状：磁盘上有、库存里没有）。
    std::fs::create_dir_all(h.acc.join("pack_hat_red")).unwrap();
    std::fs::write(h.acc.join("pack_hat_red/手工放的.png"), b"x").unwrap();

    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let err = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap_err();
    assert!(err.contains("--force"), "{err}");
    // 被拒的那一次：手工文件还在 ⇒ "拒绝覆盖"是真的没覆盖。
    assert!(h.acc.join("pack_hat_red/手工放的.png").is_file());
    assert!(!h.acc.join(pack::OWNER_DIR).exists());

    let out = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out.took_over, vec!["accessory/pack_hat_red".to_string()]);
    // 接管 = 那一目录换成包里的内容（手工文件不在了），而这件事**打印过**才允许发生。
    assert_eq!(
        h.tree(&h.acc.join("pack_hat_red")),
        vec!["accessory.json".to_string(), "sheet.webp".to_string()]
    );
}

#[test]
fn reinstalling_the_same_pack_needs_no_force_and_farms_nothing() {
    let h = Harness::new();
    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let first = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();
    assert_eq!(
        first.grant.added,
        vec!["accessory/pack_hat_red".to_string()]
    );

    let again = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();
    assert_eq!(again.grant.added, Vec::<String>::new(), "重装不该再添行");
    assert_eq!(
        again.grant.already,
        vec!["accessory/pack_hat_red".to_string()]
    );
    assert!(
        again.took_over.is_empty(),
        "自己的目录不算接管 ⇒ 不该要求 --force"
    );
    assert_eq!(
        store.item_count("pack_hat_red").unwrap(),
        1,
        "重装不 +1 ⇒ 不产碎片"
    );
}

#[test]
fn a_bundle_reaches_both_roots_with_one_ownership_list_each() {
    let h = Harness::new();
    let manifest = toml(
        "bundle_01",
        "bundle",
        &["pack_demo_cat", "pack_hat_red"],
        "itch",
        Some(990),
    );
    let path = h.zip(
        "bundle.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            ("pets/pack_demo_cat/pet.json", PET_JSON.as_bytes()),
            ("pets/pack_demo_cat/sheet.webp", SHEET),
            (
                "accessories/pack_hat_red/accessory.json",
                ACC_JSON.as_bytes(),
            ),
            ("accessories/pack_hat_red/sheet.webp", SHEET),
        ],
    );
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    assert_eq!(pack.items.len(), 2);
    assert_eq!(pack.manifest.kind, "bundle");
    assert_eq!(pack.entry_count, 5, "manifest + 两份清单 + 两份 sheet");

    let mut store = Store::in_memory().unwrap();
    let out = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();
    assert_eq!(out.roots.len(), 2, "bundle 要同时落进两个根");
    assert_eq!(out.files, 4);
    assert_eq!(
        out.grant.added,
        vec![
            "accessory/pack_hat_red".to_string(),
            "pet/pack_demo_cat".to_string()
        ]
    );
    assert!(
        out.source.starts_with("purchase:itch#local"),
        "明码标价的包不该被记成 free: {}",
        out.source
    );
    // 两个根各自有一份归属清单，各列自己那两行。
    assert_eq!(
        std::fs::read_to_string(h.pets.join(pack::OWNER_DIR).join("bundle_01")).unwrap(),
        "pack_demo_cat/pet.json\npack_demo_cat/sheet.webp\n"
    );
    assert_eq!(
        std::fs::read_to_string(h.acc.join(pack::OWNER_DIR).join("bundle_01")).unwrap(),
        "pack_hat_red/accessory.json\npack_hat_red/sheet.webp\n"
    );
    // 两个根的顶层形状各自干净 ⇒ bundle 不会把某一类的内容漏进另一类的根里。
    assert_eq!(
        h.top(&h.pets),
        vec![pack::OWNER_DIR.to_string(), "pack_demo_cat".to_string()]
    );
    assert_eq!(
        h.top(&h.acc),
        vec![pack::OWNER_DIR.to_string(), "pack_hat_red".to_string()]
    );
}

#[test]
fn a_wrong_kind_declaration_is_refused() {
    let h = Harness::new();
    // 声明 `kind = accessories` 却带了一只本体 ⇒ `ContentPack::validate` 那条 CHECK 之外的一致性。
    let manifest = toml("demo_05", "accessories", &["pack_demo_cat"], "free", None);
    let path = h.zip(
        "misdeclared.petpack",
        &[
            (pack::MANIFEST_MEMBER, manifest.as_bytes()),
            ("pets/pack_demo_cat/pet.json", PET_JSON.as_bytes()),
            ("pets/pack_demo_cat/sheet.webp", SHEET),
        ],
    );
    let err = pack::Pack::open(&path, MAX_PACK_MB).unwrap_err();
    assert!(err.contains("却带着别的内容"), "{err}");
    assert_eq!(h.top(&h.acc), Vec::<String>::new());
    assert_eq!(h.top(&h.pets), Vec::<String>::new());
}

#[test]
fn ownership_list_gates_uninstall_and_hands_made_files_survive() {
    let h = Harness::new();
    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();

    // 同根里的别的东西：另一个包装的、手工塞的、以及一个不属于本包的空目录。
    std::fs::create_dir_all(h.acc.join("stranger_hat")).unwrap();
    std::fs::write(
        h.acc.join("stranger_hat/accessory.json"),
        ACC_JSON.as_bytes(),
    )
    .unwrap();
    std::fs::create_dir_all(h.acc.join("hand_made")).unwrap();
    std::fs::write(h.acc.join("hand_made/readme.txt"), b"mine").unwrap();

    let out = pack::uninstall(&h.env, &mut store, "demo_01").unwrap();
    assert_eq!(out.removed, 2, "只删清单里那两条");
    assert_eq!(out.inventory_rows, 1);
    assert!(out.kept.is_empty());
    assert!(!h.acc.join("pack_hat_red").exists(), "本包那目录整只被剪掉");
    assert!(h.acc.join("stranger_hat/accessory.json").is_file());
    assert!(h.acc.join("hand_made/readme.txt").is_file());
    assert!(!h.acc.join(pack::OWNER_DIR).join("demo_01").exists());
    // `.installed_by` 空了才被剪；它不是本包的私有目录，还挂着别的包就必须留。
    assert_eq!(
        h.top(&h.acc),
        vec!["hand_made".to_string(), "stranger_hat".to_string()],
        "剪完不该留下任何点开头的壳"
    );
    assert!(store.owned().unwrap().is_empty());
    // `content_packs` 与 `entitlements` 原样留着：卸载 ≠ 退款（§3.4 两条不同的事）。
    assert_eq!(pack::registered(&store).unwrap().len(), 1);
    assert_eq!(pack::registered(&store).unwrap()[0].inventory_rows, 0);
}

#[test]
fn uninstalling_a_pack_that_was_never_installed_says_so() {
    let h = Harness::new();
    let mut store = Store::in_memory().unwrap();
    let err = pack::uninstall(&h.env, &mut store, "no_such_pack").unwrap_err();
    assert!(err.contains("没有名为"), "{err}");
    // 越界 id 连"查一下"都不该发生。
    assert!(pack::uninstall(&h.env, &mut store, "../etc").is_err());
}

#[test]
fn scan_roots_reports_what_is_on_disk_and_names_the_broken_ones() {
    let h = Harness::new();
    let path = h.clean_pack();
    let pack = pack::Pack::open(&path, MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();
    // 一个只有目录、没有清单的项 ⇒ 必须被点名（静默跳过就是换装页那个空列表的成因）。
    std::fs::create_dir_all(h.acc.join("no_manifest")).unwrap();

    let listings = pack::scan_roots(&h.env);
    assert_eq!(listings.len(), 2);
    let acc = listings
        .iter()
        .find(|l| l.describe.contains("accessories"))
        .expect("两条根都要出现在输出里");
    assert_eq!(acc.items, vec!["accessory/pack_hat_red".to_string()]);
    assert_eq!(acc.problems.len(), 1, "{:?}", acc.problems);
    assert!(
        acc.problems[0].contains("no_manifest"),
        "{:?}",
        acc.problems
    );
    let pets = listings
        .iter()
        .find(|l| l.describe.contains("pets"))
        .unwrap();
    assert!(pets.items.is_empty());
}

#[test]
fn entitlement_source_has_exactly_three_shapes() {
    // 从 `manifest.toml` 文本造，不手拼十二个字段：字段名与真包同源，测试才不会比实现先漂移。
    let free =
        ContentPack::from_toml(&toml("p_free", "accessories", &["x"], "free", None)).unwrap();
    let paid =
        ContentPack::from_toml(&toml("p_itch", "accessories", &["x"], "itch", Some(990))).unwrap();
    assert_eq!(
        pack::entitlement_source(&free, None),
        "free",
        "store=free 的包记 free"
    );
    assert_eq!(
        pack::entitlement_source(&paid, None),
        "purchase:itch#local",
        "明码标价的包不许被记成 free ⇒ 账面会说谎"
    );
    assert_eq!(
        pack::entitlement_source(&paid, Some("ABC123")),
        "redeem:ABC123",
        "兑换码优先于 store ⇒ 账面能追到是谁给的"
    );
}

#[test]
fn a_traversal_entry_name_never_reaches_the_installer() {
    // 写侧就拒 ⇒ 造不出一个带越界条目名的包；这条测试钉的是"越界判定只有一份实现"。
    let err = write_store_zip(&[("../escape/pet.json", PET_JSON.as_bytes())]);
    assert!(err.is_err(), "写侧必须拒绝越界条目名");
    let h = Harness::new();
    let path = h.clean_pack();
    // 正常包的条目名全部安全 ⇒ `Pack::open` 里那条判定不会误杀。
    assert!(pack::Pack::open(&path, MAX_PACK_MB).is_ok());
}

// ---- INV 门禁（FLOW §3 推论 2）----

/// 池内逐件指定 kind 的 catalog。`catalog_with` 只造配件，而 INV 要看"这一类的那条源"，
/// 所以本体侧也得有夹具（否则 kind 路由那条永远测不到）。
fn pool(pairs: &[(&str, &str)]) -> Catalog {
    let items: Vec<String> = pairs
        .iter()
        .map(|(kind, id)| match *kind {
            "pet" => format!(
                r#"{{"id":"{id}","name":"{id}","kind":"pet","rarity":"N","weight":1000}}"#
            ),
            _ => format!(
                r#"{{"id":"{id}","name":"{id}","kind":"accessory","rarity":"N","weight":1000,"slot":"head"}}"#
            ),
        })
        .collect();
    Catalog::from_json(&format!(
        r#"{{"format":1,"id":"pool_t","banner_id":"t","items":[{}]}}"#,
        items.join(",")
    ))
    .expect("夹具 catalog 要能解析")
}

#[test]
fn inv_gate_is_red_for_every_pool_item_with_no_product() {
    // **这条门禁必须能红**（§3 推论 2 的原话）。两个根都是空的 ⇒ 池内两件全缺。
    let h = Harness::new();
    let cat = pool(&[("accessory", "pack_hat_red"), ("pet", "pack_demo_cat")]);
    let rep = pack::inv_check(&h.env, &cat, None);
    assert_eq!(rep.sources.len(), 2, "配件根 + 本体根各一条");
    assert_eq!(rep.bad_items(), 2, "空库上一次都不该绿: {:?}", rep.items);
    assert!(!rep.ok());
    assert_eq!(rep.summary(), "pool=2 ok=0 missing=2");
    // 报告要给出路：点名是哪一件（`kind/id` 在 ItemCheck 上）+ 去装哪个包（catalog id 派生）。
    assert_eq!(rep.items[0].id, "pack_hat_red");
    assert!(
        rep.items[0].problems[0].contains("pool_t.petpack"),
        "缺件要说出该装哪个包: {:?}",
        rep.items[0].problems
    );
    assert_eq!(rep.pack_hint, "pool_t.petpack");
}

#[test]
fn inv_gate_turns_green_once_the_products_are_installed() {
    // 绿的那一半：装完官方免费包（它带的正是池内件）⇒ 盘上有、能解析 ⇒ 门禁绿。
    // 这条与上面那条是一对：**只有红或只有绿的门禁都算没测过**。
    let h = Harness::new();
    let cat = pool(&[("accessory", "pack_hat_red")]);
    assert_eq!(pack::inv_check(&h.env, &cat, None).bad_items(), 1);
    let pack = pack::Pack::open(&h.clean_pack(), MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    pack::install(&pack, &h.env, &mut store, &cat, &InstallOpts::default()).unwrap();
    let rep = pack::inv_check(&h.env, &cat, None);
    assert!(rep.ok(), "{:?}", rep.items);
    assert_eq!(rep.summary(), "pool=1 ok=1 missing=0");
}

#[test]
fn inv_gate_tells_a_broken_product_apart_from_a_missing_one() {
    let h = Harness::new();
    // ① 目录在、清单在、但清单说的 sheet 不在 ⇒ "产物坏了"。
    let broken = h.acc.join("broken_hat");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("accessory.json"),
        ACC_JSON.replace("\"sheet.webp\"", "\"nope.webp\""),
    )
    .unwrap();
    // ② 目录在、清单不在 ⇒ 加载器连寻址起点都没有。
    std::fs::create_dir_all(h.acc.join("no_manifest")).unwrap();
    let rep = pack::inv_check(
        &h.env,
        &pool(&[("accessory", "broken_hat"), ("accessory", "no_manifest")]),
        None,
    );
    assert_eq!(rep.bad_items(), 2);
    assert_eq!(rep.items[0].id, "broken_hat");
    assert!(
        rep.items[0].problems[0].contains("nope.webp"),
        "要说清是清单点的那个名字找不到: {:?}",
        rep.items[0].problems
    );
    assert_eq!(rep.items[1].id, "no_manifest");
    assert!(
        rep.items[1].problems[0].contains("目录在，但清单"),
        "\"有目录没清单\"与\"压根没这件\"是两句话: {:?}",
        rep.items[1].problems
    );
}

#[test]
fn inv_gate_reads_the_catalog_it_is_given() {
    // 免费包里那件在**这个** catalog 的池外 ⇒ 它写 inventory；换一份把它列进池的 catalog ⇒ 不写。
    // 钉的是"所有权判定读的是传进来的目录"，而 `opet pack install --catalog` 因此不是装饰。
    let h = Harness::new();
    let pack = pack::Pack::open(&h.clean_pack(), MAX_PACK_MB).unwrap();
    let mut store = Store::in_memory().unwrap();
    let out = pack::install(
        &pack,
        &h.env,
        &mut store,
        &empty_catalog(),
        &InstallOpts::default(),
    )
    .unwrap();
    assert_eq!(
        out.grant.added,
        vec!["accessory/pack_hat_red".to_string()],
        "池外 ⇒ 给所有权"
    );
    assert!(out.pixels_only.is_empty());

    // 同一个库、同一份包，换一份把它当池内件的 catalog ⇒ 所有权必须撤不回，所以这里用新库。
    let mut fresh = Store::in_memory().unwrap();
    let out2 = pack::install(
        &pack,
        &h.env,
        &mut fresh,
        &catalog_with(&["pack_hat_red"]),
        &InstallOpts::default(),
    )
    .unwrap();
    assert!(out2.grant.added.is_empty(), "读的是传进来的那份目录");
    assert_eq!(out2.pixels_only, vec!["accessory/pack_hat_red".to_string()]);
}

#[test]
fn inv_gate_can_take_a_dev_builtin_root_as_a_second_source() {
    // 开发态那份 `assets/` 的布局与装机后一致 ⇒ 传进来等价于"运行时会先看内置那批"。
    // 不传就只查装机根（见上面两条）⇒ 这一条钉的是 `--assets-root` 不是假开关。
    let h = Harness::new();
    let dev = h.tmp.path().join("dev_assets");
    let acc = dev.join("accessories/pack_hat_red");
    std::fs::create_dir_all(&acc).unwrap();
    std::fs::write(acc.join("accessory.json"), ACC_JSON).unwrap();
    std::fs::write(acc.join("sheet.webp"), SHEET).unwrap();
    let cat = pool(&[("accessory", "pack_hat_red")]);
    assert_eq!(pack::inv_check(&h.env, &cat, None).bad_items(), 1);
    let rep = pack::inv_check(&h.env, &cat, Some(&dev));
    assert!(rep.ok(), "{:?}", rep.items);
    assert_eq!(
        rep.sources.len(),
        4,
        "两类各两条源（内置 + 装机）⇒ 4 条，一条都不能少: {:?}",
        rep.sources
    );
}
