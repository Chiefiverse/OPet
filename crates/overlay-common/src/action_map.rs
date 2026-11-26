use petcore::behavior::AssetKey;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionMap {
    pub dbl: Option<AssetKey>,
    pub menu: Option<AssetKey>,
}

pub const EMPTY: ActionMap = ActionMap {
    dbl: None,
    menu: None,
};

pub fn parse(spec: &str) -> Result<ActionMap, String> {
    if spec.is_empty() {
        return Err("--action-map 值为空；期望形如 dbl=dance,menu=dance".into());
    }
    let mut map = ActionMap {
        dbl: None,
        menu: None,
    };
    for (i, segment) in spec.split(',').enumerate() {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(format!(
                "--action-map 第 {} 段为空（尾逗号或空节？）",
                i + 1
            ));
        }
        let eq_pos = segment.find('=').ok_or_else(|| {
            format!(
                "--action-map 段 \"{segment}\" 缺少 '='（期望 trigger=assetkey，trigger ∈ dbl|menu）"
            )
        })?;
        let trigger = &segment[..eq_pos];
        let value = &segment[eq_pos + 1..];

        if value.is_empty() {
            return Err(format!(
                "--action-map 段 \"{segment}\" 的动画名为空；可用：turn, react, dance"
            ));
        }

        let key = AssetKey::from_key(value).ok_or_else(|| {
            format!("--action-map 未知动画 \"{value}\"；本批可用：turn, react, dance")
        })?;

        if key.loops() {
            return Err(format!(
                "--action-map 动画 \"{value}\" 是循环动画，不能用作一次性叠加"
            ));
        }

        match trigger {
            "dbl" => {
                if map.dbl.is_some() {
                    return Err(format!(
                        "--action-map 触发名 \"dbl\" 重复（第 {} 段）",
                        i + 1
                    ));
                }
                map.dbl = Some(key);
            }
            "menu" => {
                if map.menu.is_some() {
                    return Err(format!(
                        "--action-map 触发名 \"menu\" 重复（第 {} 段）",
                        i + 1
                    ));
                }
                map.menu = Some(key);
            }
            other => {
                return Err(format!(
                    "--action-map 未知触发 \"{other}\"；可用：dbl, menu"
                ));
            }
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_slot_map_parses_and_names_its_keys() {
        let m = parse("dbl=dance,menu=dance").unwrap();
        assert_eq!(m.dbl, Some(AssetKey::Dance));
        assert_eq!(m.menu, Some(AssetKey::Dance));

        let m2 = parse("menu=react,dbl=turn").unwrap();
        assert_eq!(m2.dbl, Some(AssetKey::Turn));
        assert_eq!(m2.menu, Some(AssetKey::React));

        let m3 = parse("dbl=dance").unwrap();
        assert_eq!(m3.dbl, Some(AssetKey::Dance));
        assert_eq!(m3.menu, None);
    }

    #[test]
    fn every_bad_spec_is_named_and_never_defaulted() {
        // unknown asset key
        let e = parse("menu=dance2").unwrap_err();
        assert!(e.contains("dance2"), "error should name the bad value: {e}");

        // looping animation
        let e = parse("dbl=walk").unwrap_err();
        assert!(e.contains("walk"), "error should name the bad value: {e}");
        assert!(e.contains("循环"), "error should explain why: {e}");

        // unknown trigger
        let e = parse("bogus=dance").unwrap_err();
        assert!(
            e.contains("bogus"),
            "error should name the bad trigger: {e}"
        );

        // empty value
        let e = parse("dbl=").unwrap_err();
        assert!(e.contains("空"), "error about empty animation: {e}");

        // missing =
        let e = parse("dbl").unwrap_err();
        assert!(e.contains('='), "error mentions = sign: {e}");

        // empty string
        let e = parse("").unwrap_err();
        assert!(e.contains("空"), "error about empty spec: {e}");

        // trailing comma
        let e = parse("dbl=dance,").unwrap_err();
        assert!(e.contains("空"), "error about empty segment: {e}");

        // duplicate trigger
        let e = parse("dbl=turn,dbl=react").unwrap_err();
        assert!(e.contains("重复"), "error about duplicate: {e}");
    }
}
