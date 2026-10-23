//! `pet.json` 资产清单解析与校验（§3.4）。资产包是"用户可控代码"级别的输入，因此这里做严格 schema 校验（§5）。

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::behavior::AssetKey;

/// 支持的 manifest 版本上限（D4：Petdex 导入推到 v1.1，v1 只保留 `format` 版本位）。
pub const SUPPORTED_FORMAT: u32 = 2;
/// 朝向列数（D1：8 方向序列帧）。
pub const FACING_COLUMNS: usize = 8;
pub const MAX_CELL_PX: u32 = 1024;
/// 图集单边像素上限，防止一张图把内存吃穿（§5）。
pub const MAX_SHEET_PX: u32 = 8192;
pub const MAX_FPS: f32 = 60.0;

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
    pub sheet: String,
    pub grid: Grid,
    pub states: std::collections::HashMap<String, StateTrack>,
    /// `"sprite_rows_are_right_facing"`：朝向由列决定，行是状态
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
        if self.grid.rows == 0
            || (self.grid.rows as u32) * self.grid.h > MAX_SHEET_PX
            || (self.grid.cols as u32) * self.grid.w > MAX_SHEET_PX
        {
            return bad(format!("图集总尺寸过大: grid.rows={}", self.grid.rows));
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
            if let Some(max) = track.rows.iter().max() {
                if *max >= self.grid.rows {
                    return bad(format!(
                        "状态 {name} 引用行 {max} 越界（grid.rows={}）",
                        self.grid.rows
                    ));
                }
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

    /// 第 `frame` 帧、朝 `facing` 的图集单元 (row, col)。
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
        let json = MINIMAL.replace("\"format\": 2", "\"format\": 3");
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
}
