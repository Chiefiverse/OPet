//! 配件清单（D16 路线 B = **运行时逐格叠加**，不是"把配件烘进本体像素"）。
//!
//! 这条 schema 守的是一个几何事实，不是审美：**"逐格叠加的偏移恒为 0"必须是被校验出来的性质，
//! 而不是被假定的前提。** 只要配件 sheet 与本体 sheet 用同一台相机、同一批姿态烘出，
//! 两边同一 `(row, col)` 就是屏幕上同一块矩形 ⇒ 客户端直接 `blit` 到本体那一格的 dst 上即可。
//! 一旦两侧格子尺寸、列数、或某个状态的行表有任何一项漂移，那句话就不成立，而**漂移的后果
//! 在屏上表现为配件画在错位处**（不是报错）⇒ 所以 [`AccessoryManifest::validate_against`]
//! 在"装备"那一刻把两文件逐项对齐，对不上就拒绝装备。
//!
//! 与本体清单的两处刻意不同：
//! 1. **没有 `anchor` / `facing` / `input_polygon`** —— 定位完全借用本体的 feet 锚点与朝向约定
//!    （同一个相机 ⇒ 同一个锚点）。加一份自己的锚点就是"两个事实源"，正是 §9.2 第 17 条那次
//!    挂点账的成因。
//! 2. **允许只覆盖一部分状态**（`states` 可以是本体 `states` 的子集）。没覆盖的状态由**客户端**
//!    决定怎么处理（藏起来 vs 沿用最后一格），schema 只保证"声明了的状态一定同寻址"。
//!    这条不是宽松：一件只在 `idle` 烘过的帽子，在 `run` 里继续显示 `idle` 那一格就是画错。
//!
//! 生产者已存在 = `tools/pack.py --accessory <slug> --slot <head|neck> --body <本体 pet.json>`：
//! 行表**逐状态抄自本体那份清单**（不自派），因为自派行号会让一件不从 `idle` 起的配件产出一份
//! 过不了 [`AccessoryManifest::validate_against`] 的清单。本体清单这边**仍然不列配件** ——
//! 关联是单向的（配件的 `fits` 指向本体 slug），所以换装"能穿谁"由配件侧声明。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::behavior::AssetKey;
use crate::gacha::catalog::Slot;
use crate::geom::Facing;

use super::manifest::{
    FACING_COLUMNS, MAX_CELL_PX, MAX_FPS, MAX_SHEET_PX, Manifest, ManifestError, SUPPORTED_FORMAT,
    StateTrack,
};

/// 一件配件的图集清单。字段顺序即 `pack.py` 将来产出的顺序。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessoryManifest {
    pub format: u32,
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// 挂在哪一类部位上，取值与 `accessories.slot` 的 CHECK 六值同源（[`Slot`]）。
    pub slot: Slot,
    /// 为哪些宠物的相机/姿态烘的。空 = 未声明，装备时仍由 [`Self::validate_against`] 把关。
    #[serde(default)]
    pub fits: Vec<String>,
    pub sheet: String,
    /// `w/h/cols` 必须与本体逐项相等；`rows` 只需盖住自己声明的行。
    pub grid: super::manifest::Grid,
    pub states: HashMap<String, StateTrack>,
}

impl AccessoryManifest {
    pub fn parse(json: &str) -> Result<AccessoryManifest, ManifestError> {
        let m: AccessoryManifest = serde_json::from_str(json)?;
        m.validate()?;
        Ok(m)
    }

    pub fn load(path: &str) -> Result<AccessoryManifest, ManifestError> {
        let text = std::fs::read_to_string(path)?;
        AccessoryManifest::parse(&text)
    }

    /// 自身合法性（与本体清单同一套阈值，报错点名是哪件配件）。
    pub fn validate(&self) -> Result<&AccessoryManifest, ManifestError> {
        let who = || format!("配件 {}", self.slug);
        let bad = |msg: String| Err(ManifestError::Invalid(format!("{}: {msg}", who())));

        if self.format == 0 || self.format > SUPPORTED_FORMAT {
            return bad(format!(
                "format {} 超出支持范围 1..={SUPPORTED_FORMAT}",
                self.format
            ));
        }
        if self.slug.is_empty() {
            return bad("slug 为空".into());
        }
        if self.sheet.is_empty() {
            return bad("sheet 为空".into());
        }
        if self.grid.cols != FACING_COLUMNS {
            return bad(format!(
                "grid.cols 必须是 {FACING_COLUMNS}（8 方向，与本体同一套列约定），实际 {}",
                self.grid.cols
            ));
        }
        if self.grid.w == 0
            || self.grid.h == 0
            || self.grid.w > MAX_CELL_PX
            || self.grid.h > MAX_CELL_PX
        {
            return bad(format!("单帧尺寸非法 {}x{}", self.grid.w, self.grid.h));
        }
        if self.grid.rows == 0
            || (self.grid.rows as u32) * self.grid.h > MAX_SHEET_PX
            || (self.grid.cols as u32) * self.grid.w > MAX_SHEET_PX
        {
            return bad(format!("图集总尺寸过大: grid.rows={}", self.grid.rows));
        }
        if self.states.is_empty() {
            return bad("states 为空，这件配件一帧都没有".into());
        }
        for (name, track) in &self.states {
            if AssetKey::from_key(name).is_none() {
                return bad(format!("未知状态名 {name}"));
            }
            if track.rows.is_empty() {
                return bad(format!("状态 {name} 的 rows 为空"));
            }
            if !(0.0..=MAX_FPS).contains(&track.fps) {
                return bad(format!(
                    "状态 {name} 的 fps {} 非法（应在 0..={MAX_FPS}）",
                    track.fps
                ));
            }
            if let Some(max) = track.rows.iter().max()
                && *max >= self.grid.rows
            {
                return bad(format!(
                    "状态 {name} 引用行 {max} 越界（grid.rows={}）",
                    self.grid.rows
                ));
            }
        }
        Ok(self)
    }

    /// **同寻址校验** = D16 那条路线的地基。装备一件配件前必须过它。
    ///
    /// 逐项要求：格子尺寸、列数与本体相等；配件声明的每个状态，本体也必须声明，且
    /// `rows`（逐位）、解析后的 `fps`、解析后的循环标志**全部相等**。
    /// 通过后 [`Self::cell`] 对任意 `(状态, 帧, 朝向)` 必然给出与 [`Manifest::cell`] 同一个坐标。
    pub fn validate_against(&self, body: &Manifest) -> Result<&AccessoryManifest, ManifestError> {
        let who = || format!("配件 {}", self.slug);
        let bad = |msg: String| Err(ManifestError::Invalid(format!("{}: {msg}", who())));

        if !body.states.contains_key(AssetKey::Idle.as_str()) {
            // 本体清单到这里本该已经合法（`Manifest::validate` 要求 idle 存在）。
            // 走到这条说明调用方给了没校验过的清单。
            return bad("本体清单缺少 idle，无法比对".into());
        }
        // 两侧 `validate()` 都把列数钉成 `FACING_COLUMNS` ⇒ 这条对合法输入永不触发；
        // 它守的是"调用方给了未校验的清单"，与上面那条 idle 检查同族（见测试里绕开 parse 那一支）。
        if self.grid.cols != body.grid.cols {
            return bad(format!(
                "grid.cols {} 与本体的 {} 不一致 ⇒ 列不对齐，逐格叠加会横向串格",
                self.grid.cols, body.grid.cols
            ));
        }
        if self.grid.w != body.grid.w || self.grid.h != body.grid.h {
            return bad(format!(
                "格子 {}x{} 与本体的 {}x{} 不一致 ⇒ 同一格不是同一块矩形，逐格叠加的偏移不再是 0",
                self.grid.w, self.grid.h, body.grid.w, body.grid.h
            ));
        }
        if !self.fits.is_empty() && !self.fits.iter().any(|s| s == &body.slug) {
            return bad(format!(
                "fits {:?} 不包含本体 {} ⇒ 这件配件不是为这只宠物烘的",
                self.fits, body.slug
            ));
        }
        for (name, track) in &self.states {
            let Some(body_track) = body.states.get(name) else {
                return bad(format!(
                    "声明了本体没有的状态 {name} ⇒ 该状态在本体图集里没有对应的行"
                ));
            };
            if track.rows != body_track.rows {
                return bad(format!(
                    "状态 {name} 的行表 {:?} 与本体的 {:?} 不一致 ⇒ 同一帧取到不同行",
                    track.rows, body_track.rows
                ));
            }
            if track.fps.to_bits() != body_track.fps.to_bits() {
                return bad(format!(
                    "状态 {name} 的 fps {} 与本体的 {} 不一致 ⇒ 两只时钟会在中途错开帧",
                    track.fps, body_track.fps
                ));
            }
            let key = AssetKey::from_key(name)
                .ok_or_else(|| ManifestError::Invalid(format!("{}: 未知状态名 {name}", who())))?;
            if self.loops(key) != body.loops(key) {
                return bad(format!(
                    "状态 {name} 的循环标志（配件 {} / 本体 {}）不一致 ⇒ 一次性动画会有一边放完、另一边还在循环",
                    self.loops(key),
                    body.loops(key)
                ));
            }
        }
        Ok(self)
    }

    pub fn track(&self, key: AssetKey) -> Option<&StateTrack> {
        self.states.get(key.as_str())
    }

    /// 这件配件是否为本体这一档状态烘过像素。没烘过的状态**不该显示配件**（见模块头第 2 条）。
    pub fn covers(&self, key: AssetKey) -> bool {
        self.states.contains_key(key.as_str())
    }

    pub fn frame_count(&self, key: AssetKey) -> usize {
        self.track(key).map(|t| t.rows.len()).unwrap_or(0)
    }

    pub fn loops(&self, key: AssetKey) -> bool {
        self.track(key)
            .and_then(|t| t.looped)
            .unwrap_or_else(|| AssetKey::loops(key))
    }

    /// 与 [`Manifest::cell`] 逐行同构 —— 这正是 `validate_against` 之后"偏移为 0"的论证位置。
    pub fn cell(&self, key: AssetKey, frame: usize, facing: Facing) -> (usize, usize) {
        let rows = self.track(key).map(|t| t.rows.as_slice()).unwrap_or(&[]);
        let row = if rows.is_empty() {
            0
        } else {
            rows[frame % rows.len()]
        };
        (row, facing.index() as usize)
    }

    /// 配件**可能被画到**的全部图集格 `(row, col)` = 每个覆盖状态的每个行号值 × 全部列。
    ///
    /// 存在的理由只有一个：客户端要能证明配件那份 `polygon.json` 对**每一格**都有条目。
    /// 少一格 = 那一格帽子像素照样叠上屏，而它的 `wl_region` 并集里没有它 ⇒ 画出来点不到。
    /// 这种错在屏上只是"某个角度那块不跟手"，永远不会有报错，所以只能在启动时逐格查。
    ///
    /// 排序 + 去重是刻意的：`states` 是 `HashMap`，而"缺哪一格"这条报错要可重放，
    /// 不能随哈希种子变（petcore 的确定性硬约束）。
    pub fn drawn_cells(&self) -> Vec<(u32, u32)> {
        let mut cells: Vec<(u32, u32)> = self
            .states
            .values()
            .flat_map(|t| {
                t.rows.iter().flat_map(move |&row| {
                    (0..self.grid.cols as u32).map(move |col| (row as u32, col))
                })
            })
            .collect();
        cells.sort_unstable();
        cells.dedup();
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::default_manifest;

    /// 与 `cat_shiro` 同相机同姿态烘出来的一顶帽子：格子 192×192、8 列、只覆盖 `idle`。
    const HAT: &str = r#"{
      "format": 2, "slug": "cap_blue", "display_name": "蓝帽",
      "slot": "head", "fits": ["cat_shiro"],
      "sheet": "sheet.webp",
      "grid": { "cols": 8, "rows": 4, "w": 192, "h": 192 },
      "states": { "idle": {"rows": [0, 1, 2, 3], "fps": 8, "loop": true} }
    }"#;

    fn hat() -> AccessoryManifest {
        AccessoryManifest::parse(HAT).expect("文档例必须可解析")
    }

    fn body() -> Manifest {
        default_manifest()
    }

    #[test]
    fn parses_documented_example() {
        let a = hat();
        assert_eq!(a.slug, "cap_blue");
        assert_eq!(a.slot, Slot::Head);
        assert!(a.covers(AssetKey::Idle));
        assert!(!a.covers(AssetKey::Run), "没烘过的状态要读得出『没烘过』");
        a.validate_against(&body()).expect("同寻址必须放行");
    }

    /// **D16 的核心性质**：过了 `validate_against`，任意 `(状态, 帧, 朝向)` 的取格与本体逐位相同
    /// ⇒ 客户端叠加时不需要任何偏移，`outfit_anchors` 在这条路上不是输入。
    #[test]
    fn an_agreeing_accessory_never_selects_a_different_cell() {
        let a = hat();
        let b = body();
        a.validate_against(&b).unwrap();
        let mut checked_states = 0;
        for key in AssetKey::ALL {
            if !a.covers(key) {
                continue;
            }
            checked_states += 1;
            // 帧数故意越过本体的帧数：越界两侧靠同一个余数取格，仍要逐位相同
            for frame in 0..(b.frame_count(key) + 2) {
                for f in Facing::ALL {
                    assert_eq!(
                        a.cell(key, frame, f),
                        b.cell(key, frame, f),
                        "{} 第 {frame} 帧、col {}",
                        key.as_str(),
                        f.index()
                    );
                    assert_eq!(a.cell(key, frame, f).1, f.index() as usize, "列就是朝向");
                }
            }
        }
        assert_eq!(checked_states, 1, "这份夹具只该覆盖 idle 一档");
    }

    #[test]
    fn rejects_a_different_cell_size() {
        let json = HAT.replace("\"w\": 192, \"h\": 192", "\"w\": 96, \"h\": 96");
        let a = AccessoryManifest::parse(&json).expect("自身合法（96 也是合法格子）");
        let e = a
            .validate_against(&body())
            .expect_err("格子尺寸不同的配件必须被拒绝装备");
        let msg = e.to_string();
        assert!(msg.contains("cap_blue"), "错误要点名是哪件配件: {msg}");
        assert!(msg.contains("偏移不再是 0"), "错误要说出后果: {msg}");
    }

    /// 列数在本项目里是**全局常量**：两侧各自的 `validate()` 都钉死 `FACING_COLUMNS = 8`，
    /// 所以走 `parse` 进来的清单永远到不了 `validate_against` 的那条比对 ⇒ 先证明"8 列"这条
    /// 硬约定在解析期就守住了。
    #[test]
    fn rejects_a_column_count_other_than_eight_at_parse() {
        let json = HAT.replace("\"cols\": 8", "\"cols\": 4");
        assert!(AccessoryManifest::parse(&json).is_err(), "8 列是硬约定");
    }

    /// …再证明 `validate_against` 里那条 `grid.cols` 比对**不是死代码**：绕开 `parse` 直接改
    /// 结构体，它必须接住。这条存在的理由不是"今天会漂移"，而是"哪天放开 4 列配件（D11 排除过
    /// 的那个提案）时，它是唯一还站着的一道"。
    #[test]
    fn rejects_a_mismatched_column_count_when_parse_is_bypassed() {
        let mut a = hat();
        a.grid.cols = 4;
        let msg = a
            .validate_against(&body())
            .expect_err("列数不一致必须被拒绝装备")
            .to_string();
        assert!(msg.contains("列不对齐"), "{msg}");
    }

    /// 行表"少一帧"也必须拒绝：`rows` 变短后同一个 frame 索引落到不同物理行，
    /// 而这在屏上只是配件稍微慢半拍 —— 没有任何像素门禁会报。
    #[test]
    fn rejects_a_shorter_row_table() {
        let json = HAT.replace("[0, 1, 2, 3]", "[0, 1, 2]");
        let a = AccessoryManifest::parse(&json).unwrap();
        let msg = a.validate_against(&body()).err().unwrap().to_string();
        assert!(msg.contains("行表"), "{msg}");
    }

    #[test]
    fn rejects_a_different_fps() {
        let json = HAT.replace("\"fps\": 8", "\"fps\": 12");
        let a = AccessoryManifest::parse(&json).unwrap();
        let msg = a.validate_against(&body()).err().unwrap().to_string();
        assert!(msg.contains("fps"), "{msg}");
    }

    /// 循环标志一边显式一边靠默认值时，解析后的值仍要相等才能放行；反过来把它写反必须拒绝。
    #[test]
    fn rejects_a_different_loop_flag() {
        let implied = HAT.replace(", \"loop\": true", "");
        AccessoryManifest::parse(&implied)
            .unwrap()
            .validate_against(&body())
            .expect("默认值推出来一样 ⇒ 放行");
        let flipped = HAT.replace("\"loop\": true", "\"loop\": false");
        let msg = AccessoryManifest::parse(&flipped)
            .unwrap()
            .validate_against(&body())
            .err()
            .unwrap()
            .to_string();
        assert!(msg.contains("循环标志"), "{msg}");
    }

    /// 本体只烘了 `idle` 一档（M2 首批产物就是这个形状）。
    fn body_idle_only() -> Manifest {
        Manifest::parse(
            r#"{
              "format": 2, "slug": "cat_shiro", "display_name": "小白",
              "sheet": "sheet.webp",
              "grid": { "cols": 8, "rows": 4, "w": 192, "h": 192 },
              "states": { "idle": {"rows": [0, 1, 2, 3], "fps": 8, "loop": true} },
              "anchor": { "feet": [96, 180] }
            }"#,
        )
        .expect("单状态子集清单合法")
    }

    /// 帽子把 `idle` + `walk` 都烘了（`walk` 的行表与全量本体逐位相同）。
    fn hat_idle_and_walk() -> AccessoryManifest {
        let json = HAT.replace(
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}, \"walk\": {\"rows\": [4, 5, 6, 7, 8, 9, 10, 11], \"fps\": 12, \"loop\": true}",
        ).replace("\"rows\": 4,", "\"rows\": 12,");
        AccessoryManifest::parse(&json).expect("两档帽子自身合法")
    }

    /// **交叉校验这一支真的会红**：状态名合法、自身校验放行，但本体图集里没有那一档的行 ⇒
    /// 装备必须被拒绝（否则 `walk` 时配件会从本体的第 4 行起取像素，画的是别人）。
    #[test]
    fn rejects_a_state_the_body_does_not_have() {
        let msg = hat_idle_and_walk()
            .validate_against(&body_idle_only())
            .expect_err("本体没烘 walk，烘了 walk 的配件不能装备")
            .to_string();
        assert!(msg.contains("walk"), "要点名是哪一档: {msg}");
        assert!(msg.contains("没有对应的行"), "要说出后果: {msg}");
    }

    /// 状态名写错（哪怕只是多一个字母）在**解析期**就该被挡下：`AssetKey` 是封闭枚举。
    #[test]
    fn rejects_an_unknown_state_name_at_parse() {
        for bad in ["idles", "fly"] {
            let json = HAT.replace("\"idle\"", &format!("\"{bad}\""));
            assert!(
                AccessoryManifest::parse(&json).is_err(),
                "状态名 {bad} 不是 AssetKey，必须解析失败"
            );
        }
    }

    #[test]
    fn rejects_a_row_out_of_its_own_sheet() {
        let json = HAT.replace("[0, 1, 2, 3]", "[0, 1, 2, 4]");
        let e = AccessoryManifest::parse(&json).err().unwrap();
        let msg = e.to_string();
        assert!(msg.contains("越界"), "{msg}");
        assert!(msg.contains("cap_blue"), "{msg}");
    }

    #[test]
    fn rejects_a_state_list_that_is_only_empty_rows() {
        let json = HAT.replace("[0, 1, 2, 3]", "[]");
        assert!(AccessoryManifest::parse(&json).is_err());
    }

    #[test]
    fn rejects_an_accessory_baked_for_another_pet() {
        let json = HAT.replace("[\"cat_shiro\"]", "[\"tiger\"]");
        let a = AccessoryManifest::parse(&json).unwrap();
        let msg = a.validate_against(&body()).err().unwrap().to_string();
        assert!(msg.contains("tiger"), "要报出 fits 里写了谁: {msg}");
    }

    #[test]
    fn empty_fits_defers_to_the_paired_check() {
        let json = HAT.replace(", \"fits\": [\"cat_shiro\"]", "");
        let a = AccessoryManifest::parse(&json).unwrap();
        assert!(a.fits.is_empty());
        a.validate_against(&body())
            .expect("未声明 fits ⇒ 只靠同寻址那几项把关");
    }

    /// 允许多档子集：帽子把 `idle` + `walk` 都烘了，行表逐位相同 ⇒ 放行。
    #[test]
    fn allows_a_subset_of_states_when_it_agrees() {
        let json = HAT.replace(
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}, \"walk\": {\"rows\": [4, 5, 6, 7, 8, 9, 10, 11], \"fps\": 12, \"loop\": true}",
        ).replace("\"rows\": 4,", "\"rows\": 12,");
        let a = AccessoryManifest::parse(&json).unwrap();
        a.validate_against(&body()).expect("两档子集必须放行");
        assert!(a.covers(AssetKey::Walk));
        assert_eq!(a.frame_count(AssetKey::Walk), 8);
    }

    /// 反面对照：多档子集里只要有一档行表漂移，整件配件就要被拒。
    #[test]
    fn rejects_when_only_the_second_state_drifts() {
        let json = HAT.replace(
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}, \"walk\": {\"rows\": [4, 5, 6, 7, 8, 9, 10], \"fps\": 12, \"loop\": true}",
        ).replace("\"rows\": 4,", "\"rows\": 12,");
        let a = AccessoryManifest::parse(&json).unwrap();
        let msg = a.validate_against(&body()).err().unwrap().to_string();
        assert!(msg.contains("walk"), "要点名是哪一档漂移: {msg}");
    }

    #[test]
    fn slot_values_share_the_catalog_enum() {
        // 逐 `Slot::ALL` 成员解析，而不是在这里再抄一份字符串表：抄的那份在下一次加值时
        // 不会报错，只会**静默少测一个**（v2 加 `feet` 正是下一次）。
        // schema 那一侧的同源断言在 `catalog.rs::slot_and_kind_strings_match_the_ddl_check_constraints`。
        for good in Slot::ALL {
            let name = good.as_str();
            let json = HAT.replace("\"head\"", &format!("\"{name}\""));
            AccessoryManifest::parse(&json)
                .unwrap_or_else(|e| panic!("slot {name} 必须可解析: {e}"));
        }
        let json = HAT.replace("\"head\"", "\"tail\"");
        assert!(
            AccessoryManifest::parse(&json).is_err(),
            "catalog 没有 tail 这一档挂点"
        );
    }

    /// 以下几档把 `validate()` 里剩下的每一个 `bad` 分支逐个点名。它们看着琐碎，但
    /// **"某分支没有任何测试"与"某分支永远到不了"在屏上是同一种失败**（本轮已经抓到后者一次）。
    #[test]
    fn rejects_an_unsupported_format_version() {
        for bad in ["0", "3"] {
            let json = HAT.replace("\"format\": 2", &format!("\"format\": {bad}"));
            assert!(
                AccessoryManifest::parse(&json).is_err(),
                "format {bad} 不该被接受"
            );
        }
    }

    #[test]
    fn rejects_an_empty_identifier_or_sheet() {
        assert!(AccessoryManifest::parse(&HAT.replace("\"cap_blue\"", "\"\"")).is_err());
        assert!(AccessoryManifest::parse(&HAT.replace("\"sheet.webp\"", "\"\"")).is_err());
    }

    #[test]
    fn rejects_an_empty_state_map() {
        let json = HAT.replace(
            "\"states\": { \"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true} }",
            "\"states\": {}",
        );
        assert!(
            AccessoryManifest::parse(&json).is_err(),
            "一帧都没有的配件不是配件"
        );
    }

    #[test]
    fn rejects_an_out_of_range_fps() {
        let json = HAT.replace("\"fps\": 8", "\"fps\": 600");
        assert!(
            AccessoryManifest::parse(&json).is_err(),
            "fps 越界必须拒绝（与本体同一阈值）"
        );
    }

    /// 与 `grid.cols` 那一支同族：`Manifest::validate` 要求本体有 `idle`，所以走 `parse`
    /// 进来的本体永远不缺 ⇒ 这条守的是"调用方给了未校验的本体清单"。绕开 parse 才够得着 ⇒
    /// 用它证明这不是死分支。
    #[test]
    fn refuses_to_pair_with_an_unvalidated_body_manifest() {
        let mut b = body();
        b.states.remove(AssetKey::Idle.as_str());
        let msg = hat()
            .validate_against(&b)
            .expect_err("拿不到 idle 就没法比对，必须拒绝而不是放行")
            .to_string();
        assert!(msg.contains("缺少 idle"), "{msg}");
    }

    #[test]
    fn drawn_cells_enumerates_every_column_of_every_covered_row() {
        let cells = hat().drawn_cells();
        assert_eq!(cells.len(), 4 * 8, "rows [0,1,2,3] × 8 列");
        assert!(cells.is_sorted(), "报错要可重放 ⇒ 顺序稳定");
        assert_eq!(cells.first(), Some(&(0, 0)));
        assert_eq!(cells.last(), Some(&(3, 7)));
        assert!(
            !cells.iter().any(|&(r, _)| r >= 4),
            "不能越出配件自己的行域"
        );
    }

    #[test]
    fn drawn_cells_dedupes_and_ignores_the_hashmap_iteration_order() {
        // 两个状态共用行号是畸形资产，但 `drawn_cells` 的职责只是枚举 ⇒ 去重就够。
        // 这条真正钉的是**确定性**：`states` 是 `HashMap`，若输出顺序随种子变，
        // 客户端那句"缺哪一格"每次启动会点名不同的格子（同 petcore 的确定性硬约束）。
        let json = HAT.replace(
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0, 1], \"fps\": 8, \"loop\": true}, \
             \"walk\": {\"rows\": [1, 2], \"fps\": 12, \"loop\": true}",
        );
        let a = AccessoryManifest::parse(&json).expect("两状态夹具要能解析");
        let cells = a.drawn_cells();
        assert_eq!(cells.len(), 3 * 8, "行 {{0,1,2}} 去重后 3 行 × 8 列");
        assert_eq!(cells, a.drawn_cells());
    }
}
