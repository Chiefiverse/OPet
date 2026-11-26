//! `pet.json` 资产清单解析与校验（§3.4）。资产包是"用户可控代码"级别的输入，因此这里做严格 schema 校验（§5）。

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::behavior::AssetKey;

/// 支持的 manifest 版本上限（D4：Petdex 导入推到 v1.1，v1 只保留 `format` 版本位）。
/// **3 = 行号全局连续 + 像素分段存放（`sheets`）**，判据见 DESIGN.md §6.1.27。
pub const SUPPORTED_FORMAT: u32 = 3;
/// 配件侧还没有分段装载 ⇒ 配件清单只接受到 2（见 [`crate::assets::AccessoryManifest::validate`]）。
pub(crate) const ACCESSORY_MAX_FORMAT: u32 = 2;
/// 朝向列数（D1：8 方向序列帧）。
pub const FACING_COLUMNS: usize = 8;
pub const MAX_CELL_PX: u32 = 1024;
/// 图集单边像素上限，防止一张图把内存吃穿（§5）。
pub const MAX_SHEET_PX: u32 = 8192;
pub const MAX_FPS: f32 = 60.0;

/// 像素文件名必须是**裸文件名**：不含任何分隔符（堵死分隔符就等于堵死绝对路径，两个平台
/// 的前缀形式都带分隔符），不含 `:`（Windows 的盘符相对 `C:x` 与 NTFS 流名 `x:stream`
/// 都不带分隔符，是另一条出路），也不是 `.` / `..`。
///
/// 自多 sheet 起，装载方（`overlay_common::atlas::AtlasSet`、配件的 `decode`）拿的
/// 就是清单里的名字，而它们一律 `dir.join(name)`。拼死 `"sheet.webp"` 时代不存在
/// 的问题，现在存在了：一份写着 `../../etc/passwd` 的清单会让客户端去读资产目录之外
/// 的文件。资产包成员那一条早就有同口径的穿越拒绝（`assets/source.rs`），这里是同一
/// 条判据落在"从盘上直读"这一侧。
pub(crate) fn is_bare_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
        && name != "."
        && name != ".."
}

#[derive(Debug)]
pub enum ManifestError {
    Json(serde_json::Error),
    Io(std::io::Error),
    Invalid(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Json(e) => write!(f, "pet.json 解析失败: {e}"),
            ManifestError::Io(e) => write!(f, "pet.json 读取失败: {e}"),
            ManifestError::Invalid(msg) => write!(f, "pet.json 校验失败: {msg}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<serde_json::Error> for ManifestError {
    fn from(e: serde_json::Error) -> Self {
        ManifestError::Json(e)
    }
}

impl From<std::io::Error> for ManifestError {
    fn from(e: std::io::Error) -> Self {
        ManifestError::Io(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub w: u32,
    pub h: u32,
}

/// 一段像素：`file` 是资产目录里的相对文件名，`rows` 是这段占用的**全局行数**。
/// 段与段按声明顺序首尾相接 ⇒ 重叠与缺口在这种表示里无法写出，只剩"总和不等于 `grid.rows`"
/// 一种坏形状，由 [`Manifest::validate`] 拦住（DESIGN §6.1.27 裁决 2）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SheetDesc {
    pub file: String,
    pub rows: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateTrack {
    /// 该状态占用的图集物理行；一帧一行，`rows.len()` 即帧数。
    /// 单帧状态（Petdex 风格）写成 `"rows": [0]`。
    pub rows: Vec<usize>,
    pub fps: f32,
    /// 缺省时由 [`AssetKey::loops`] 决定（idle/walk/run/climb/sleep 循环，其余一次性）。
    /// §3.4 的示例里键名是 `"loop"`，因此保留为 alias。
    #[serde(default, alias = "loop")]
    pub looped: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub slug: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// 单张存放时的像素文件名。带 `sheets` 时必须留空 ⇒ 像素事实源只能有一个。
    #[serde(default)]
    pub sheet: String,
    /// `grid.rows` = **逻辑总行数**（全局行号空间），不是某一张文件的行数。
    pub grid: Grid,
    pub states: std::collections::HashMap<String, StateTrack>,
    /// 朝向由列决定，行是状态
    #[serde(default)]
    pub facing: Option<String>,
    pub anchor: std::collections::HashMap<String, [f32; 2]>,
    #[serde(default)]
    pub outfit_anchors: std::collections::HashMap<String, [f32; 2]>,
    /// v1 只支持 `"auto_from_alpha"`（多边形由 pack.py 从 alpha 通道算出）
    #[serde(default)]
    pub input_polygon: Option<String>,
    #[serde(default)]
    pub petdex_compatible: bool,
    /// 像素分段存放。**空 = 单张（`sheet`）的旧路径**；非空 ⇒ 必须 `format = 3`、
    /// `sheet` 必须留空，且各段 `rows` 之和 == `grid.rows`。行号永远全局连续，
    /// 段内行号只由 [`Manifest::locate`] 现算。
    #[serde(default)]
    pub sheets: Vec<SheetDesc>,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Manifest, ManifestError> {
        let m: Manifest = serde_json::from_str(json)?;
        m.validate()?;
        Ok(m)
    }

    pub fn load(path: &str) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path)?;
        Manifest::parse(&text)
    }

    /// 校验后返回自身，便于链式调用。
    pub fn validate(&self) -> Result<&Manifest, ManifestError> {
        let bad = |msg: String| Err(ManifestError::Invalid(msg));

        if self.format == 0 || self.format > SUPPORTED_FORMAT {
            return bad(format!(
                "format {} 超出支持范围 1..={SUPPORTED_FORMAT}",
                self.format
            ));
        }
        if self.slug.is_empty() {
            return bad("slug 为空".into());
        }
        if self.grid.cols != FACING_COLUMNS {
            return bad(format!(
                "grid.cols 必须是 {FACING_COLUMNS}（8 方向），实际 {}",
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
        if self.grid.rows == 0 || (self.grid.cols as u32) * self.grid.w > MAX_SHEET_PX {
            return bad(format!(
                "图集尺寸非法: grid.rows={} grid.cols*grid.w={}",
                self.grid.rows,
                (self.grid.cols as u32) * self.grid.w
            ));
        }
        if self.sheets.is_empty() {
            if self.format == SUPPORTED_FORMAT {
                return bad(format!(
                    "format {SUPPORTED_FORMAT} 就是「分段存放」这一档，必须同时给出非空 sheets（否则老读者会按单张装载出错误像素）"
                ));
            }
            if self.sheet.is_empty() {
                return bad("单张存放必须给出 sheet 文件名".into());
            }
            if !is_bare_name(&self.sheet) {
                return bad(format!(
                    "sheet 文件名 \"{}\" 不是裸文件名：装载方一律 `dir.join(name)`，含分隔符或 `:` 会读到资产目录之外",
                    self.sheet
                ));
            }
            // 旧路径：grid.rows 同时是那张文件的行数 ⇒ 42 行那一类的顶仍然逐字有效。
            if (self.grid.rows as u32) * self.grid.h > MAX_SHEET_PX {
                return bad(format!(
                    "图集过高: grid.rows={} × grid.h={} > {MAX_SHEET_PX}",
                    self.grid.rows, self.grid.h
                ));
            }
        } else {
            self.validate_sheets()?;
        }
        if !self.states.contains_key(AssetKey::Idle.as_str()) {
            return bad("缺少 idle 状态，宠物无法待机".into());
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
                && self.locate(*max).is_none()
            {
                return bad(format!(
                    "状态 {name} 引用行 {max} 落不进任何一张（grid.rows={}，分段 {:?}）",
                    self.grid.rows,
                    self.sheets.iter().map(|s| s.rows).collect::<Vec<_>>()
                ));
            }
        }
        let Some(feet) = self.anchor.get("feet") else {
            return bad("anchor.feet 缺失".into());
        };
        if feet[0] < 0.0
            || feet[1] < 0.0
            || feet[0] > self.grid.w as f32
            || feet[1] > self.grid.h as f32
        {
            return bad(format!("anchor.feet {feet:?} 不在帧内"));
        }
        for (slot, off) in &self.outfit_anchors {
            if !off[0].is_finite() || !off[1].is_finite() {
                return bad(format!("outfit anchor {slot} 含非有限值"));
            }
            if off[0] < 0.0
                || off[1] < 0.0
                || off[0] > self.grid.w as f32
                || off[1] > self.grid.h as f32
            {
                return bad(format!("outfit anchor {slot} {off:?} 不在帧内"));
            }
        }
        match self.input_polygon.as_deref() {
            None | Some("auto_from_alpha") => {}
            Some(other) => {
                return bad(format!(
                    "input_polygon 只支持 auto_from_alpha，收到 {other}"
                ));
            }
        }
        if let Some(f) = &self.facing
            && f != "sprite_rows_are_right_facing"
        {
            return bad(format!(
                "facing 只支持 sprite_rows_are_right_facing，收到 {f}"
            ));
        }
        Ok(self)
    }

    /// 分段存放那一档的专属校验（只在 `sheets` 非空时调用）。
    fn validate_sheets(&self) -> Result<(), ManifestError> {
        let bad = |msg: String| Err(ManifestError::Invalid(msg));
        if self.format != SUPPORTED_FORMAT {
            return bad(format!(
                "带 sheets 的清单必须是 format {SUPPORTED_FORMAT}，实际 {} ⇒ 更低版本的读者会忽略 sheets、按单张 sheet 装载出错误像素",
                self.format
            ));
        }
        if !self.sheet.is_empty() {
            return bad(format!(
                "带 sheets 时不许再写 sheet（\"{}\"）⇒ 像素会有两个事实源",
                self.sheet
            ));
        }
        let mut total = 0usize;
        let mut names = std::collections::BTreeSet::new();
        for (i, s) in self.sheets.iter().enumerate() {
            if s.file.is_empty() {
                return bad(format!("sheets[{i}] 没有文件名"));
            }
            if !is_bare_name(&s.file) {
                return bad(format!(
                    "sheets[{i}] 的文件名 \"{}\" 不是裸文件名：装载方一律 `dir.join(name)`，含分隔符或 `:` 会读到资产目录之外",
                    s.file
                ));
            }
            if !names.insert(s.file.as_str()) {
                return bad(format!("sheets[{i}] 的文件名 \"{}\" 与前一段重复", s.file));
            }
            if s.rows == 0 {
                return bad(format!(
                    "sheets[{i}]（{}）的 rows 为 0 ⇒ 空段会让行号平移",
                    s.file
                ));
            }
            if (s.rows as u32) * self.grid.h > MAX_SHEET_PX {
                return bad(format!(
                    "sheets[{i}]（{}）过高: {} 行 × {} px > {MAX_SHEET_PX}",
                    s.file, s.rows, self.grid.h
                ));
            }
            total += s.rows;
        }
        if total != self.grid.rows {
            let layout = self
                .sheets
                .iter()
                .map(|s| s.rows.to_string())
                .collect::<Vec<_>>()
                .join("+");
            return bad(format!(
                "sheets 必须正好平铺全局行号: 各段行数 {layout} = {total}，grid.rows={}",
                self.grid.rows
            ));
        }
        Ok(())
    }

    /// **全仓唯一的换算口**：全局行号 → `(sheet 序号, 段内行号)`；越界返回 `None`。
    /// 段序号即 [`Manifest::sheet_files`] 的下标（DESIGN §6.1.27 裁决 1）。
    pub fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if self.sheets.is_empty() {
            return if row < self.grid.rows {
                Some((0, row))
            } else {
                None
            };
        }
        let mut base = 0usize;
        for (i, s) in self.sheets.iter().enumerate() {
            if row < base + s.rows {
                return Some((i, row - base));
            }
            base += s.rows;
        }
        None
    }

    /// 全局行号空间的大小（= `grid.rows`；分段时它等于各段 `rows` 之和）。
    pub fn total_rows(&self) -> usize {
        self.grid.rows
    }

    /// 要装载的像素文件名，顺序与 [`Manifest::locate`] 返回的段序号一致。
    pub fn sheet_files(&self) -> Vec<&str> {
        self.sheet_segments().into_iter().map(|(f, _)| f).collect()
    }

    /// 逐张的 `(文件名, 该张行数)` = **装载方要的几何输入**，顺序同段序号。
    ///
    /// 单张存放返回一项 `(sheet, grid.rows)`；分段返回每张自己声明的行数。
    /// 这个函数存在的意义：装载方拿的是**每张自己的高度**去校验解码尺寸，
    /// 而不是拿全局 `grid.rows`（那样第二张必然报到"尺寸不符"，而那是对的读数吗 —— 不是，
    /// 那是我们把两个数混成一个的错）。行数的唯一出处仍是这里，不是各客户端自己算。
    pub fn sheet_segments(&self) -> Vec<(&str, usize)> {
        if self.sheets.is_empty() {
            vec![(self.sheet.as_str(), self.grid.rows)]
        } else {
            self.sheets
                .iter()
                .map(|s| (s.file.as_str(), s.rows))
                .collect()
        }
    }

    pub fn track(&self, key: AssetKey) -> Option<&StateTrack> {
        self.states.get(key.as_str())
    }

    pub fn has(&self, key: AssetKey) -> bool {
        self.states.contains_key(key.as_str())
    }

    pub fn frame_count(&self, key: AssetKey) -> usize {
        self.track(key).map(|t| t.rows.len()).unwrap_or(0)
    }

    /// 该状态是否循环播放：manifest 显式写了 `loop` 就以它为准，否则用 `AssetKey` 的默认。
    pub fn loops(&self, key: AssetKey) -> bool {
        self.track(key)
            .and_then(|t| t.looped)
            .unwrap_or_else(|| AssetKey::loops(key))
    }

    pub fn fps(&self, key: AssetKey) -> f32 {
        self.track(key).map(|t| t.fps).unwrap_or(8.0)
    }

    /// 第 `frame` 帧、朝 `facing` 的图集单元 `(row, col)`。
    /// `row` 是**全局行号**；它落在哪一张由 [`Manifest::locate`] 现算，这里不参与。
    pub fn cell(&self, key: AssetKey, frame: usize, facing: crate::geom::Facing) -> (usize, usize) {
        let rows = self.track(key).map(|t| t.rows.as_slice()).unwrap_or(&[]);
        let row = if rows.is_empty() {
            0
        } else {
            rows[frame % rows.len()]
        };
        (row, facing.index() as usize)
    }

    pub fn feet_anchor(&self) -> [f32; 2] {
        self.anchor
            .get("feet")
            .copied()
            .unwrap_or([self.grid.w as f32 / 2.0, self.grid.h as f32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
      "format": 2, "slug": "cat_shiro", "display_name": "小白",
      "sheet": "sheet.webp", "grid": { "cols": 8, "rows": 9, "w": 128, "h": 128 },
      "states": { "idle": {"rows": [0], "fps": 8, "loop": true} },
      "facing": "sprite_rows_are_right_facing",
      "anchor": {"feet": [64, 120]},
      "input_polygon": "auto_from_alpha",
      "petdex_compatible": true
    }"#;

    #[test]
    fn parses_documented_example() {
        let m = Manifest::parse(MINIMAL).unwrap();
        assert_eq!(m.slug, "cat_shiro");
        assert_eq!(m.frame_count(AssetKey::Idle), 1);
        assert!(m.loops(AssetKey::Idle));
        assert_eq!(m.cell(AssetKey::Idle, 0, crate::geom::Facing::West), (0, 4));
        assert_eq!(m.feet_anchor(), [64.0, 120.0]);
    }

    #[test]
    fn rejects_bad_format_version() {
        let json = MINIMAL.replace("\"format\": 2", "\"format\": 4");
        assert!(matches!(
            Manifest::parse(&json),
            Err(ManifestError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_wrong_column_count() {
        let json = MINIMAL.replace("\"cols\": 8", "\"cols\": 4");
        assert!(Manifest::parse(&json).is_err());
    }

    #[test]
    fn rejects_row_out_of_range() {
        let json = MINIMAL.replace("\"rows\": [0]", "\"rows\": [9]");
        assert!(Manifest::parse(&json).is_err());
    }

    #[test]
    fn rejects_unknown_state_name() {
        let json = MINIMAL.replace("\"idle\"", "\"fly\"");
        assert!(Manifest::parse(&json).is_err());
    }

    #[test]
    fn rejects_unknown_input_polygon_mode() {
        let json = MINIMAL.replace("auto_from_alpha", "from_depth_buffer");
        assert!(Manifest::parse(&json).is_err());
    }

    /// 挂点必须落在帧内：挂件的贴图是按这些坐标定位的，越界就是"配件画到帧外"，
    /// 而图集之外的像素不存在 ⇒ 客户端只能画空。§9.2 第 13 条"挂件看不见"的地基。
    #[test]
    fn rejects_outfit_anchor_outside_the_frame() {
        let with_anchor = MINIMAL.replace(
            "\"input_polygon\"",
            "\"outfit_anchors\": {\"head\": [64, 20]}, \"input_polygon\"",
        );
        Manifest::parse(&with_anchor).expect("帧内的挂点必须放行（正面对照）");
        for out_of_frame in ["[129, 20]", "[-1, 20]", "[64, 129]", "[64, -1]"] {
            let json = with_anchor.replace("[64, 20]", out_of_frame);
            let err = Manifest::parse(&json)
                .err()
                .unwrap_or_else(|| panic!("越界挂点 {out_of_frame} 被放行了（grid 是 128×128）"));
            let msg = err.to_string();
            assert!(msg.contains("head"), "错误要点名是哪个挂点: {msg}");
        }
    }

    #[test]
    fn multi_frame_state_lists_rows() {
        let json = MINIMAL.replace(
            "\"idle\": {\"rows\": [0], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0, 1, 2, 3], \"fps\": 8, \"loop\": true}",
        );
        let m = Manifest::parse(&json).unwrap();
        assert_eq!(m.frame_count(AssetKey::Idle), 4);
        assert_eq!(m.cell(AssetKey::Idle, 5, crate::geom::Facing::East), (1, 0));
    }

    #[test]
    fn one_shot_state_is_not_looping() {
        let json = MINIMAL.replace(
            "\"idle\": {\"rows\": [0], \"fps\": 8, \"loop\": true}",
            "\"idle\": {\"rows\": [0], \"fps\": 8, \"loop\": true}, \"stop\": {\"rows\": [3, 4, 5], \"fps\": 12, \"loop\": false}",
        );
        let m = Manifest::parse(&json).unwrap();
        assert!(!m.loops(AssetKey::Stop));
        assert!(m.loops(AssetKey::Idle));
        // 没有显式声明的状态用 AssetKey 默认（react 是一次性）
        assert!(!m.loops(AssetKey::React));
    }

    /// 分段存放的正样本：全局行号 0..9 拆成 `0..5` + `5..9` 两张。
    /// 行表 `states.*.rows` **与单张时代逐字相同** —— 这正是裁决 1 要证明的事。
    const SPLIT: &str = r#"{
      "format": 3, "slug": "cat_shiro", "display_name": "小白",
      "grid": { "cols": 8, "rows": 9, "w": 128, "h": 128 },
      "states": {
        "idle": {"rows": [0], "fps": 8, "loop": true},
        "walk": {"rows": [3, 4, 5], "fps": 8, "loop": true},
        "run": {"rows": [6, 7, 8], "fps": 12, "loop": true}
      },
      "facing": "sprite_rows_are_right_facing",
      "anchor": {"feet": [64, 120]},
      "input_polygon": "auto_from_alpha",
      "sheets": [ {"file": "sheet_0.webp", "rows": 5}, {"file": "sheet_1.webp", "rows": 4} ]
    }"#;

    fn sheets_of(json: &str) -> Vec<(String, usize)> {
        Manifest::parse(json)
            .unwrap_or_else(|e| panic!("样本应当合法: {e}"))
            .sheets
            .iter()
            .map(|s| (s.file.clone(), s.rows))
            .collect()
    }

    /// 平铺 = 各段行数之和正好盖住全局行号空间。段是首尾相接的，所以"重叠"与"缺口"
    /// 在这种表示里写不出来 ⇒ 坏形状只剩总和不等、空段、重名三种（设计上的收口）。
    #[test]
    fn sheets_must_tile_grid_rows_exactly() {
        let ok = Manifest::parse(SPLIT).unwrap();
        assert_eq!(
            sheets_of(SPLIT),
            vec![
                ("sheet_0.webp".to_owned(), 5),
                ("sheet_1.webp".to_owned(), 4)
            ]
        );
        assert_eq!(ok.total_rows(), 9);

        for (bad_layout, why) in [
            (
                r#"[ {"file": "a.webp", "rows": 5}, {"file": "b.webp", "rows": 3} ]"#,
                "总和太小（缺口）",
            ),
            (
                r#"[ {"file": "a.webp", "rows": 5}, {"file": "b.webp", "rows": 5} ]"#,
                "总和太大",
            ),
            (
                r#"[ {"file": "a.webp", "rows": 9}, {"file": "b.webp", "rows": 0} ]"#,
                "空段",
            ),
            (
                r#"[ {"file": "a.webp", "rows": 5}, {"file": "a.webp", "rows": 4} ]"#,
                "重名",
            ),
            (r#"[ {"file": "", "rows": 9} ]"#, "没有文件名"),
            (
                r"[]",
                "空数组由 sheets.is_empty() 走旧路径，此处只验写法边界",
            ),
        ] {
            let json = SPLIT.replace(
                r#""sheets": [ {"file": "sheet_0.webp", "rows": 5}, {"file": "sheet_1.webp", "rows": 4} ]"#,
                &format!("\"sheets\": {bad_layout}"),
            );
            let outcome = Manifest::parse(&json);
            if why.contains("旧路径") {
                // `sheets: []` 等价于没写 ⇒ 走单张校验，而单张路径要求 sheet 非空。
                assert!(
                    outcome.is_err(),
                    "空 sheets + 无 sheet 字段不该被当成合法: {why}"
                );
                continue;
            }
            let err = outcome
                .err()
                .unwrap_or_else(|| panic!("坏 sheets {bad_layout} 被放行（{why}）"));
            let msg = err.to_string();
            assert!(
                msg.contains("sheet") || msg.contains("平铺"),
                "报错要能让排错的人看出是 sheets 的问题: {msg}"
            );
        }
    }

    /// 装载方（`AtlasSet::load` / 配件 `decode`）拿的是清单里的名字并且一律 `dir.join(name)`
    /// ⇒ 名字必须是裸文件名。这一族判据要**两条路径各钉一次**：`sheet` 与 `sheets[].file`
    /// 是两个字段，只测一个的话另一侧的穿越没人拦（而另一侧正是新造出来的那条）。
    #[test]
    fn a_sheet_name_must_be_bare_on_both_storage_paths() {
        // 反斜杠在 JSON 里是转义引导符，写进这份循环会变成"解析失败"而不是"名字违规"
        // ⇒ 它单独走谓词那一层（两个字段共用同一个谓词，测一次等价）。
        for name in [
            "../etc/passwd",
            "/etc/passwd",
            "sub/sheet.webp",
            "x:stream",
            "..",
            ".",
        ] {
            let single = MINIMAL.replace(
                "\"sheet\": \"sheet.webp\"",
                &format!("\"sheet\": \"{name}\""),
            );
            let err = Manifest::parse(&single)
                .err()
                .unwrap_or_else(|| panic!("单张路径放行了 {name:?}"));
            assert!(
                err.to_string().contains("裸文件名"),
                "报错要说清是哪一条规则（否则改资产的人以为是拼写问题）: {err}"
            );

            let split = SPLIT.replace(
                r#""file": "sheet_0.webp""#,
                &format!("\"file\": \"{name}\""),
            );
            let err = Manifest::parse(&split)
                .err()
                .unwrap_or_else(|| panic!("分段路径放行了 {name:?}"));
            assert!(err.to_string().contains("裸文件名"), "{err}");
        }
        // 正面对照：合法的改名（含点、连字符、unicode）必须仍然放行，否则这条门禁
        // 红的是自己（同一族里"把阈值改成 0"式的假防护）。
        for name in ["sheet-01.webp", "Sheet A.webp", "猫.webp", "a.min.webp"] {
            let single = MINIMAL.replace(
                "\"sheet\": \"sheet.webp\"",
                &format!("\"sheet\": \"{name}\""),
            );
            assert!(
                Manifest::parse(&single).is_ok(),
                "合法的裸文件名被拒: {name}"
            );
        }
        // 谓词本身：反斜杠（Windows 分隔符）与空串都算不裸 —— 这两形到不了上面那个循环，
        // 前者被 JSON 转义吃掉，后者另有"没有文件名"那条更早的判据。
        assert!(!is_bare_name("sheet\\a.webp"));
        assert!(!is_bare_name(""));
        assert!(is_bare_name("sheet.webp"), "最常见的名字必须放行");
    }

    /// 状态行表必须落在某一段里。平铺合法 + `row < grid.rows` 在这套表示下等价，
    /// 但**判据走 `locate`**（唯一的换算口），且报错要点名状态名、行号与分段布局。
    #[test]
    fn every_state_row_lands_inside_one_sheet() {
        let ok = Manifest::parse(SPLIT).unwrap();
        for key in [AssetKey::Idle, AssetKey::Walk, AssetKey::Run] {
            for row in ok.track(key).unwrap().rows.clone() {
                assert!(
                    ok.locate(row).is_some(),
                    "行 {row}（{key:?}）落不进任何一张"
                );
            }
        }
        let json = SPLIT.replace(
            "\"run\": {\"rows\": [6, 7, 8]",
            "\"run\": {\"rows\": [6, 7, 9]",
        );
        let err = Manifest::parse(&json).expect_err("行 9 在全局行号 0..9 之外，必须被拒");
        let msg = err.to_string();
        assert!(msg.contains("run"), "报错要点名状态: {msg}");
        assert!(msg.contains('9'), "报错要点名行号: {msg}");
        assert!(
            msg.contains("4"),
            "报错要带上分段布局，便于看出差在哪一段: {msg}"
        );
    }

    /// `format` 与 `sheets` 必须同进同出：这一条堵的是**假门禁** ——
    /// 老读者的 `serde(default)` 会静默忽略 sheets 并按单张装载 ⇒ 错像素且不报错。
    #[test]
    fn a_v3_manifest_without_sheets_is_refused_and_v2_never_reads_sheets() {
        // 3 而无 sheets ⇒ 拒
        let v3_no_sheets = SPLIT.replace(
            r#",
      "sheets": [ {"file": "sheet_0.webp", "rows": 5}, {"file": "sheet_1.webp", "rows": 4} ]"#,
            "",
        );
        let err = Manifest::parse(&v3_no_sheets).expect_err("format 3 而无 sheets 必须被拒");
        assert!(err.to_string().contains('3'), "报错要说明是版本档: {err}");

        // 2 而带 sheets ⇒ 拒（老读者会忽略 sheets）
        let v2_with_sheets = SPLIT.replace("\"format\": 3", "\"format\": 2");
        let err = Manifest::parse(&v2_with_sheets).expect_err("format 2 带 sheets 必须被拒");
        assert!(
            err.to_string().contains("sheets"),
            "报错要点名 sheets: {err}"
        );

        // 3 + sheets + 还写着 sheet ⇒ 拒（两个像素事实源）
        let two_sources =
            SPLIT.replace(r#""sheets": [ "#, r#""sheet": "sheet.webp", "sheets": [ "#);
        let err = Manifest::parse(&two_sources).expect_err("sheets 与 sheet 同时存在必须被拒");
        assert!(
            err.to_string().contains("两个事实源"),
            "报错要说明为什么: {err}"
        );

        // 旧清单逐字不动：单张路径仍认 sheet，locate 是恒等映射
        let legacy = Manifest::parse(MINIMAL).unwrap();
        assert_eq!(legacy.sheet_files(), vec!["sheet.webp"]);
        assert_eq!(legacy.locate(0), Some((0, 0)));
        assert_eq!(legacy.locate(8), Some((0, 8)));
        assert_eq!(legacy.locate(9), None);
        assert_eq!(legacy.total_rows(), 9);
    }

    /// `locate` 是全局行号 → `(段序号, 段内行号)` 的唯一换算口；`cell()` 仍返回全局坐标。
    #[test]
    fn locate_is_the_only_global_row_mapping() {
        let m = Manifest::parse(SPLIT).unwrap();
        assert_eq!(m.sheet_files(), vec!["sheet_0.webp", "sheet_1.webp"]);
        for (row, want) in [
            (0, (0, 0)),
            (4, (0, 4)), // 第一段的最后一行
            (5, (1, 0)), // 跨段边界：新的一段从 0 重新数
            (8, (1, 3)), // 最后一段的最后一行
        ] {
            assert_eq!(m.locate(row), Some(want), "全局行 {row} 的归属");
        }
        assert_eq!(m.locate(9), None);
        assert_eq!(m.locate(99), None);

        // 列（朝向）与单张时代完全同构，行仍是全局号
        assert_eq!(m.cell(AssetKey::Run, 2, crate::geom::Facing::West), (8, 4));
        for key in AssetKey::ALL {
            for frame in 0..m.frame_count(key) {
                let (row, col) = m.cell(key, frame, crate::geom::Facing::SouthEast);
                let (sheet, row_in_sheet) = m
                    .locate(row)
                    .unwrap_or_else(|| panic!("{key:?} 第 {frame} 帧落在行 {row}，无段可归"));
                assert_eq!(col, crate::geom::Facing::SouthEast.index() as usize);
                assert!(row_in_sheet < m.sheets[sheet].rows);
            }
        }

        // 三段也算得对（不是只给两段写死的）
        let three = SPLIT.replace(
            r#""sheets": [ {"file": "sheet_0.webp", "rows": 5}, {"file": "sheet_1.webp", "rows": 4} ]"#,
            r#""sheets": [ {"file": "a.webp", "rows": 3}, {"file": "b.webp", "rows": 3}, {"file": "c.webp", "rows": 3} ]"#,
        );
        let m3 = Manifest::parse(&three).unwrap();
        assert_eq!(m3.locate(0), Some((0, 0)));
        assert_eq!(m3.locate(3), Some((1, 0)));
        assert_eq!(m3.locate(8), Some((2, 2)));

        // 装载方要的**逐张几何**与段序号同序，且行数与 `locate` 用的是同一份事实。
        // ⚠️ 这条钉的是"每张自己的高度"：拿全局 `grid.rows` 去校验第二张必然红，
        // 而那个红会被读成"产物坏了"—— 病其实在我们这边。
        assert_eq!(
            m3.sheet_segments(),
            vec![("a.webp", 3), ("b.webp", 3), ("c.webp", 3)]
        );
        assert_eq!(
            m.sheet_segments(),
            vec![("sheet_0.webp", 5), ("sheet_1.webp", 4)]
        );
        let mut base = 0usize;
        for (i, (file, rows)) in m.sheet_segments().iter().enumerate() {
            assert!(!file.is_empty(), "第 {i} 张的名字不能是空的");
            assert_eq!(
                m.locate(base),
                Some((i, 0)),
                "第 {i} 张的第一行 = 全局行 {base}"
            );
            assert_eq!(
                m.locate(base + rows - 1),
                Some((i, rows - 1)),
                "第 {i} 张的最后一行 = 全局行 {}",
                base + rows - 1
            );
            assert_eq!(
                m.locate(base + rows).filter(|(s, _)| *s == i),
                None,
                "第 {i} 张占的行数就是 {rows}，多一行都不许算进它头上"
            );
            base += rows;
        }
        assert_eq!(base, m.total_rows(), "逐张行数之和 == 全局行号空间");
    }

    /// 本批的存在理由：**42 行那道顶从"行号空间"降为"逐文件"**。
    /// 同一份 60 行行表，单张写法仍被 MAX_SHEET_PX 拦住，分段写法必须放行。
    #[test]
    fn the_row_ceiling_follows_the_file_not_the_row_space() {
        let body = r#"{
          "format": 2, "slug": "tiger",
          "sheet": "sheet.webp", "grid": { "cols": 8, "rows": 60, "w": 192, "h": 192 },
          "states": { "idle": {"rows": [0, 1, 2], "fps": 8, "loop": true}, "sleep": {"rows": [57, 58, 59], "fps": 4, "loop": true} },
          "anchor": {"feet": [96, 188]}
        }"#;
        let err = Manifest::parse(body)
            .expect_err("60 行 × 192 px = 11,520 > 8,192，单张必须仍然被拦（否则这道闸就白写了）");
        assert!(
            err.to_string().contains("过高"),
            "报错要说明是高度问题: {err}"
        );

        let split = body
            .replace("\"format\": 2", "\"format\": 3")
            .replace(
                r#""sheet": "sheet.webp", "#,
                r#""sheets": [ {"file": "sheet_0.webp", "rows": 30}, {"file": "sheet_1.webp", "rows": 30} ], "#,
            );
        let m =
            Manifest::parse(&split).unwrap_or_else(|e| panic!("同样的 60 行分段存放应当合法: {e}"));
        assert_eq!(m.total_rows(), 60);
        assert_eq!(m.locate(59), Some((1, 29)));

        // 逐文件那一闸仍然有效：单段 45 行 × 192 = 8,640 > 8,192
        let too_tall_one_segment = split.replace(
            r#""sheets": [ {"file": "sheet_0.webp", "rows": 30}, {"file": "sheet_1.webp", "rows": 30} ]"#,
            r#""sheets": [ {"file": "sheet_0.webp", "rows": 45}, {"file": "sheet_1.webp", "rows": 15} ]"#,
        );
        let err = Manifest::parse(&too_tall_one_segment)
            .expect_err("某一段自己超过 MAX_SHEET_PX 必须被点名拒绝");
        assert!(
            err.to_string().contains("sheet_0.webp"),
            "报错要说是哪一张: {err}"
        );
    }
}
