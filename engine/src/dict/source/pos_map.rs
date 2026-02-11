/// Sudachi → Mozc POS ID remapping.
///
/// Sudachi and Mozc use different POS tag systems and ID numbering.
/// This module normalizes Sudachi POS tags to the Mozc convention and
/// builds a lookup table (sudachi_id → mozc_id) so that Sudachi dictionary
/// entries can use the Mozc connection matrix.
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::DictSourceError;
use crate::dict::DictEntry;

/// Parse Mozc `id.def` and return a map from POS tag string to Mozc ID.
///
/// Format: `<id> <pos1>,<pos2>,<pos3>,<pos4>,<conj_type>,<conj_form>,<vocab>`
///
/// Only entries with `vocab == "*"` (generic) are included; vocabulary-specific
/// entries are skipped because we want the general POS ID.
pub fn parse_mozc_id_def(path: &Path) -> Result<HashMap<String, u16>, DictSourceError> {
    let content = fs::read_to_string(path).map_err(DictSourceError::Io)?;
    let mut map = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Split into "<id> <pos_tag>"
        let (id_str, pos_tag) = line
            .split_once(' ')
            .ok_or_else(|| DictSourceError::Parse(format!("invalid id.def line: {line}")))?;
        let id: u16 = id_str
            .parse()
            .map_err(|_| DictSourceError::Parse(format!("invalid id in id.def: {id_str}")))?;

        // Parse the 7-field POS tag
        let fields: Vec<&str> = pos_tag.split(',').collect();
        if fields.len() != 7 {
            continue;
        }
        // Only keep generic entries (vocab == "*")
        if fields[6] != "*" {
            continue;
        }
        // Store with vocab field intact (always "*")
        map.insert(pos_tag.to_string(), id);
    }

    Ok(map)
}

/// Scan Sudachi CSV files and determine the most frequent POS tag for each
/// left_id, then build a sudachi_id → mozc_id remap table.
///
/// Returns `(remap_table, matched_count, total_unique_ids)`.
pub fn build_remap_table(
    csv_dir: &Path,
    mozc_ids: &HashMap<String, u16>,
) -> Result<(HashMap<u16, u16>, usize, usize), DictSourceError> {
    // Collect POS tag frequency for each sudachi left_id.
    // Key: sudachi left_id, Value: map of POS tag → count
    let mut id_pos_counts: HashMap<u16, HashMap<String, usize>> = HashMap::new();

    let mut files: Vec<_> = fs::read_dir(csv_dir)
        .map_err(DictSourceError::Io)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".csv"))
        .collect();
    files.sort_by_key(|e| e.file_name());

    for file_entry in &files {
        let content = fs::read_to_string(file_entry.path()).map_err(DictSourceError::Io)?;
        for line in content.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split(',').collect();
            // Need at least cols 0..=10 (surface, left_id, right_id, cost, ?, pos1-4, conj_type, conj_form)
            if fields.len() < 11 {
                continue;
            }
            let left_id: u16 = match fields[1].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Skip special IDs
            if fields[1] == "-1" {
                continue;
            }
            // POS fields: 5=品詞大分類, 6=品詞中分類, 7=品詞小分類, 8=品詞細分類, 9=活用型, 10=活用形
            let pos_tag = format!(
                "{},{},{},{},{},{}",
                fields[5], fields[6], fields[7], fields[8], fields[9], fields[10]
            );
            *id_pos_counts
                .entry(left_id)
                .or_default()
                .entry(pos_tag)
                .or_insert(0) += 1;
        }
    }

    // For each sudachi left_id, find the most frequent POS tag, normalize it,
    // and look up in the Mozc ID map.
    let mut remap: HashMap<u16, u16> = HashMap::new();
    let total = id_pos_counts.len();
    let mut matched = 0;

    for (sudachi_id, pos_counts) in &id_pos_counts {
        // Find most frequent POS tag for this ID
        let best_pos = pos_counts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(pos, _)| pos.as_str())
            .unwrap();

        let fields: Vec<&str> = best_pos.split(',').collect();
        if fields.len() != 6 {
            continue;
        }
        let pos6: [&str; 6] = [
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        ];

        // Generate normalized POS candidates (most specific → most general)
        let candidates = normalize_sudachi_pos(&pos6);

        let mut found = false;
        for candidate in &candidates {
            if let Some(&mozc_id) = mozc_ids.get(candidate) {
                remap.insert(*sudachi_id, mozc_id);
                matched += 1;
                found = true;
                break;
            }
        }
        if !found {
            // Ultimate fallback: 名詞,一般
            if let Some(&fallback_id) = mozc_ids.get("名詞,一般,*,*,*,*,*") {
                remap.insert(*sudachi_id, fallback_id);
                matched += 1;
            }
        }
    }

    Ok((remap, matched, total))
}

/// Apply the remap table to all dictionary entries, replacing left_id and right_id.
pub fn remap_entries(entries: &mut HashMap<String, Vec<DictEntry>>, remap: &HashMap<u16, u16>) {
    for entry_list in entries.values_mut() {
        for entry in entry_list.iter_mut() {
            if let Some(&new_id) = remap.get(&entry.left_id) {
                entry.left_id = new_id;
            }
            if let Some(&new_id) = remap.get(&entry.right_id) {
                entry.right_id = new_id;
            }
        }
    }
}

/// Normalize a Sudachi 6-field POS tag to a list of Mozc 7-field POS candidates.
///
/// `pos` fields: [品詞大分類, 品詞中分類, 品詞小分類, 品詞細分類, 活用型, 活用形]
///
/// Returns candidates from most specific to most general. The caller should try
/// each one against the Mozc id.def map and use the first match.
fn normalize_sudachi_pos(pos: &[&str; 6]) -> Vec<String> {
    let pos1 = pos[0]; // 品詞大分類
    let pos2 = pos[1]; // 品詞中分類
    let pos3 = pos[2]; // 品詞小分類
    let pos4 = pos[3]; // 品詞細分類
    let conj_type = pos[4]; // 活用型
    let conj_form = pos[5]; // 活用形

    // Convert POS categories
    let (m_pos1, m_pos2, m_pos3, m_pos4) = convert_pos_category(pos1, pos2, pos3, pos4);
    let m_conj_type = convert_conjugation_type(conj_type);
    let m_conj_form = convert_conjugation_form(conj_form);

    let mut candidates = Vec::new();

    // 1. Exact normalized match (vocab=*)
    candidates.push(format!(
        "{m_pos1},{m_pos2},{m_pos3},{m_pos4},{m_conj_type},{m_conj_form},*"
    ));

    // 2. Relax conjugation form
    if m_conj_form != "*" {
        candidates.push(format!(
            "{m_pos1},{m_pos2},{m_pos3},{m_pos4},{m_conj_type},*,*"
        ));
    }

    // 3. Relax conjugation type
    if m_conj_type != "*" {
        candidates.push(format!("{m_pos1},{m_pos2},{m_pos3},{m_pos4},*,*,*"));
    }

    // 4. Relax pos3 and pos4
    if m_pos3 != "*" || m_pos4 != "*" {
        candidates.push(format!(
            "{m_pos1},{m_pos2},*,*,{m_conj_type},{m_conj_form},*"
        ));
        candidates.push(format!("{m_pos1},{m_pos2},*,*,{m_conj_type},*,*"));
        candidates.push(format!("{m_pos1},{m_pos2},*,*,*,*,*"));
    }

    // 5. Relax pos2
    if m_pos2 != "*" {
        candidates.push(format!("{m_pos1},*,*,*,{m_conj_type},{m_conj_form},*"));
        candidates.push(format!("{m_pos1},*,*,*,*,*,*"));
    }

    candidates
}

/// Convert Sudachi POS category fields to Mozc equivalents.
fn convert_pos_category<'a>(
    pos1: &'a str,
    pos2: &'a str,
    pos3: &'a str,
    _pos4: &'a str,
) -> (&'a str, &'a str, &'a str, &'a str) {
    match (pos1, pos2, pos3) {
        // 代名詞 → 名詞,代名詞,一般
        ("代名詞", _, _) => ("名詞", "代名詞", "一般", "*"),
        // 形状詞,タリ → 名詞,ナイ形容詞語幹
        ("形状詞", "タリ", _) => ("名詞", "ナイ形容詞語幹", "*", "*"),
        // 形状詞,一般 / 形状詞,* → 名詞,形容動詞語幹
        ("形状詞", _, _) => ("名詞", "形容動詞語幹", "*", "*"),
        // 接頭辞 → 接頭詞,名詞接続
        ("接頭辞", _, _) => ("接頭詞", "名詞接続", "*", "*"),
        // 接尾辞,名詞的,助数詞 → 名詞,接尾,助数詞
        ("接尾辞", "名詞的", "助数詞") => ("名詞", "接尾", "助数詞", "*"),
        // 接尾辞,名詞的,* → 名詞,接尾,一般
        ("接尾辞", "名詞的", _) => ("名詞", "接尾", "一般", "*"),
        // 接尾辞,形状詞的 → 名詞,接尾,一般
        ("接尾辞", "形状詞的", _) => ("名詞", "接尾", "一般", "*"),
        // 接尾辞,動詞的 → 動詞,接尾
        ("接尾辞", "動詞的", _) => ("動詞", "接尾", "*", "*"),
        // 接尾辞,形容詞的 → 形容詞,接尾
        ("接尾辞", "形容詞的", _) => ("形容詞", "接尾", "*", "*"),
        // 接尾辞,* → 名詞,接尾,一般 (general fallback)
        ("接尾辞", _, _) => ("名詞", "接尾", "一般", "*"),
        // 補助記号 → 記号,一般
        ("補助記号", _, _) => ("記号", "一般", "*", "*"),
        // 名詞,普通名詞,サ変可能 → 名詞,サ変接続
        ("名詞", "普通名詞", "サ変可能") => ("名詞", "サ変接続", "*", "*"),
        // 名詞,普通名詞,副詞可能 → 名詞,副詞可能
        ("名詞", "普通名詞", "副詞可能") => ("名詞", "副詞可能", "*", "*"),
        // 名詞,普通名詞,助数詞可能 → 名詞,接尾,助数詞
        ("名詞", "普通名詞", "助数詞可能") => ("名詞", "接尾", "助数詞", "*"),
        // 名詞,普通名詞,形状詞可能 → 名詞,形容動詞語幹
        ("名詞", "普通名詞", "形状詞可能") => ("名詞", "形容動詞語幹", "*", "*"),
        // 名詞,普通名詞,一般 → 名詞,一般
        ("名詞", "普通名詞", "一般") => ("名詞", "一般", "*", "*"),
        // 名詞,普通名詞,* → 名詞,一般 (catch-all for other 普通名詞)
        ("名詞", "普通名詞", _) => ("名詞", "一般", "*", "*"),
        // 名詞,数詞 → 名詞,数
        ("名詞", "数詞", _) => ("名詞", "数", "*", "*"),
        // 名詞,固有名詞 → 名詞,固有名詞 (keep subcategories)
        ("名詞", "固有名詞", _) => ("名詞", "固有名詞", "一般", "*"),
        // 動詞,一般 → 動詞,自立
        ("動詞", "一般", _) => ("動詞", "自立", "*", "*"),
        // 動詞,非自立可能 → 動詞,非自立
        ("動詞", "非自立可能", _) => ("動詞", "非自立", "*", "*"),
        // 形容詞,非自立可能 → 形容詞,非自立
        ("形容詞", "非自立可能", _) => ("形容詞", "非自立", "*", "*"),
        // 形容詞,一般 → 形容詞,自立
        ("形容詞", "一般", _) => ("形容詞", "自立", "*", "*"),
        // Pass through anything else
        _ => (pos1, pos2, pos3, _pos4),
    }
}

/// Convert Sudachi conjugation type to Mozc equivalent.
fn convert_conjugation_type(conj_type: &str) -> &str {
    match conj_type {
        "五段-カ行" => "五段・カ行イ音便",
        "五段-ガ行" => "五段・ガ行",
        "五段-サ行" => "五段・サ行",
        "五段-タ行" => "五段・タ行",
        "五段-ナ行" => "五段・ナ行",
        "五段-バ行" => "五段・バ行",
        "五段-マ行" => "五段・マ行",
        "五段-ラ行" => "五段・ラ行",
        "五段-ワア行" => "五段・ワ行ウ音便",
        "サ行変格" => "サ変・スル",
        "カ行変格" => "カ変・来ル",
        "形容詞" => "形容詞・アウオ段",
        "文語形容詞-ク" => "形容詞・アウオ段",
        s if s.starts_with("上一段-") => "一段",
        s if s.starts_with("下一段-") => "一段",
        _ => conj_type,
    }
}

/// Convert Sudachi conjugation form to Mozc equivalent.
fn convert_conjugation_form(conj_form: &str) -> &str {
    match conj_form {
        "終止形-一般" | "連体形-一般" => "基本形",
        "連用形-一般" => "連用形",
        "連用形-イ音便" | "連用形-促音便" | "連用形-撥音便" | "連用形-ウ音便" => {
            "連用タ接続"
        }
        "連用形-融合" => "仮定縮約１",
        "仮定形-一般" => "仮定形",
        "仮定形-融合" => "仮定縮約１",
        "命令形" => "命令ro",
        "意志推量形" => "未然ウ接続",
        "未然形-一般" | "未然形-撥音便" => "未然形",
        "未然形-セ" => "未然レル接続",
        "語幹-一般" => "体言接続",
        _ => conj_form,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id_def(lines: &[&str]) -> String {
        lines.join("\n")
    }

    #[test]
    fn test_parse_mozc_id_def() {
        let dir = std::env::temp_dir().join("lexime_test_id_def");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.def");
        fs::write(
            &path,
            make_id_def(&[
                "0 BOS/EOS,*,*,*,*,*,*",
                "1852 名詞,一般,*,*,*,*,*",
                "1842 名詞,サ変接続,*,*,*,*,*",
                "680 動詞,自立,*,*,一段,基本形,*",
                "681 動詞,自立,*,*,一段,基本形,食べる", // vocab-specific, should be skipped
            ]),
        )
        .unwrap();

        let map = parse_mozc_id_def(&path).unwrap();
        assert_eq!(map.get("名詞,一般,*,*,*,*,*"), Some(&1852));
        assert_eq!(map.get("名詞,サ変接続,*,*,*,*,*"), Some(&1842));
        assert_eq!(map.get("動詞,自立,*,*,一段,基本形,*"), Some(&680));
        // Vocab-specific entry should be excluded
        assert!(!map.contains_key("動詞,自立,*,*,一段,基本形,食べる"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_normalize_noun_general() {
        let pos: [&str; 6] = ["名詞", "普通名詞", "一般", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,一般,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_noun_sahen() {
        let pos: [&str; 6] = ["名詞", "普通名詞", "サ変可能", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,サ変接続,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_pronoun() {
        let pos: [&str; 6] = ["代名詞", "*", "*", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,代名詞,一般,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_keijoshi_tari() {
        let pos: [&str; 6] = ["形状詞", "タリ", "*", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,ナイ形容詞語幹,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_keijoshi_general() {
        let pos: [&str; 6] = ["形状詞", "一般", "*", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,形容動詞語幹,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_verb_godan_ka() {
        let pos: [&str; 6] = ["動詞", "一般", "*", "*", "五段-カ行", "連体形-一般"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"動詞,自立,*,*,五段・カ行イ音便,基本形,*".to_string()));
    }

    #[test]
    fn test_normalize_verb_ichidan() {
        let pos: [&str; 6] = ["動詞", "一般", "*", "*", "上一段-ア行", "終止形-一般"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"動詞,自立,*,*,一段,基本形,*".to_string()));
    }

    #[test]
    fn test_normalize_verb_shimoidchidan() {
        let pos: [&str; 6] = ["動詞", "一般", "*", "*", "下一段-バ行", "連用形-一般"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"動詞,自立,*,*,一段,連用形,*".to_string()));
    }

    #[test]
    fn test_normalize_adjective() {
        let pos: [&str; 6] = ["形容詞", "一般", "*", "*", "形容詞", "終止形-一般"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"形容詞,自立,*,*,形容詞・アウオ段,基本形,*".to_string()));
    }

    #[test]
    fn test_normalize_suffix_noun() {
        let pos: [&str; 6] = ["接尾辞", "名詞的", "一般", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,接尾,一般,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_suffix_counter() {
        let pos: [&str; 6] = ["接尾辞", "名詞的", "助数詞", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"名詞,接尾,助数詞,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_prefix() {
        let pos: [&str; 6] = ["接頭辞", "*", "*", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"接頭詞,名詞接続,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_supplementary_symbol() {
        let pos: [&str; 6] = ["補助記号", "一般", "*", "*", "*", "*"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"記号,一般,*,*,*,*,*".to_string()));
    }

    #[test]
    fn test_normalize_verb_renyou_onbin() {
        // 連用形-促音便 should map to 連用タ接続
        let pos: [&str; 6] = ["動詞", "一般", "*", "*", "五段-ラ行", "連用形-促音便"];
        let candidates = normalize_sudachi_pos(&pos);
        assert!(candidates.contains(&"動詞,自立,*,*,五段・ラ行,連用タ接続,*".to_string()));
    }

    #[test]
    fn test_normalize_fallback_relaxation() {
        // A made-up POS should still produce fallback candidates
        let pos: [&str; 6] = ["動詞", "一般", "*", "*", "unknown-type", "unknown-form"];
        let candidates = normalize_sudachi_pos(&pos);
        // Should include a relaxed candidate without conj_form
        assert!(candidates
            .iter()
            .any(|c| c == "動詞,自立,*,*,unknown-type,*,*"));
        // Should include a fully relaxed candidate
        assert!(candidates.iter().any(|c| c == "動詞,自立,*,*,*,*,*"));
    }

    #[test]
    fn test_build_remap_table() {
        let dir = std::env::temp_dir().join("lexime_test_remap");
        fs::create_dir_all(&dir).unwrap();

        // Create a small Sudachi CSV
        fs::write(
            dir.join("test.csv"),
            "漢字,100,100,5000,漢字,名詞,普通名詞,一般,*,*,*,カンジ,漢字,*,A,*,*,*,*\n\
             感じ,100,100,5100,感じ,名詞,普通名詞,一般,*,*,*,カンジ,感じ,*,A,*,*,*,*\n\
             食べる,200,200,4000,食べる,動詞,一般,*,*,下一段-バ行,終止形-一般,タベル,食べる,*,A,*,*,*,*\n",
        )
        .unwrap();

        // Create a small Mozc id.def
        let id_path = dir.join("id.def");
        fs::write(
            &id_path,
            "1852 名詞,一般,*,*,*,*,*\n\
             680 動詞,自立,*,*,一段,基本形,*\n",
        )
        .unwrap();

        let mozc_ids = parse_mozc_id_def(&id_path).unwrap();
        let (remap, matched, total) = build_remap_table(&dir, &mozc_ids).unwrap();

        assert_eq!(total, 2); // IDs 100 and 200
        assert_eq!(matched, 2);
        assert_eq!(remap.get(&100), Some(&1852)); // 名詞,普通名詞,一般 → 名詞,一般
        assert_eq!(remap.get(&200), Some(&680)); // 動詞,一般,下一段 → 動詞,自立,一段

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_remap_entries() {
        let mut entries: HashMap<String, Vec<DictEntry>> = HashMap::new();
        entries.insert(
            "かんじ".to_string(),
            vec![DictEntry {
                surface: "漢字".to_string(),
                cost: 5000,
                left_id: 100,
                right_id: 100,
            }],
        );

        let mut remap = HashMap::new();
        remap.insert(100u16, 1852u16);

        remap_entries(&mut entries, &remap);

        let e = &entries["かんじ"][0];
        assert_eq!(e.left_id, 1852);
        assert_eq!(e.right_id, 1852);
    }
}
