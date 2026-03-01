/// Mozc id.def parser and POS utilities.
use std::fs;
use std::path::Path;

use super::DictSourceError;

/// Parse Mozc `id.def` and return a map from POS tag string to Mozc ID.
///
/// Format: `<id> <pos1>,<pos2>,<pos3>,<pos4>,<conj_type>,<conj_form>,<vocab>`
///
/// Only entries with `vocab == "*"` (generic) are included; vocabulary-specific
/// entries are skipped because we want the general POS ID.
pub fn parse_mozc_id_def(
    path: &Path,
) -> Result<std::collections::HashMap<String, u16>, DictSourceError> {
    let content = fs::read_to_string(path).map_err(DictSourceError::Io)?;
    let mut map = std::collections::HashMap::new();

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

/// Extract the contiguous ID range for function words (助詞 and 助動詞) from Mozc `id.def`.
///
/// Returns `(min_id, max_id)`. If no matching entries are found, returns `(0, 0)`.
pub fn function_word_id_range(path: &Path) -> Result<(u16, u16), DictSourceError> {
    let content = fs::read_to_string(path).map_err(DictSourceError::Io)?;
    let mut ids = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (id_str, pos_tag) = line
            .split_once(' ')
            .ok_or_else(|| DictSourceError::Parse(format!("invalid id.def line: {line}")))?;
        if pos_tag.starts_with("助動詞,") || pos_tag.starts_with("助詞,") {
            let id: u16 = id_str
                .parse()
                .map_err(|_| DictSourceError::Parse(format!("invalid id: {id_str}")))?;
            ids.push(id);
        }
    }

    if ids.is_empty() {
        return Ok((0, 0));
    }

    let min = *ids.iter().min().unwrap();
    let max = *ids.iter().max().unwrap();
    Ok((min, max))
}

/// Morpheme role constants for bunsetsu segmentation.
pub const ROLE_CONTENT_WORD: u8 = 0;
pub const ROLE_FUNCTION_WORD: u8 = 1;
pub const ROLE_SUFFIX: u8 = 2;
pub const ROLE_PREFIX: u8 = 3;
pub const ROLE_NON_INDEPENDENT: u8 = 4;
pub const ROLE_PRONOUN: u8 = 5;
pub const ROLE_PERSON_NAME: u8 = 6;

/// Parse Mozc `id.def` and classify each POS ID into a morpheme role.
///
/// Returns a `Vec<u8>` indexed by POS ID, where each value is one of:
/// - `0` (ContentWord): default — nouns, verbs, adjectives, adverbs, etc.
/// - `1` (FunctionWord): 助詞 or 助動詞
/// - `2` (Suffix): second POS field is 接尾 (e.g., 名詞,接尾 / 動詞,接尾 / 形容詞,接尾)
/// - `3` (Prefix): first POS field is 接頭詞
/// - `4` (NonIndependent): 非自立 (e.g., 名詞,非自立 / 動詞,非自立 / 形容詞,非自立)
/// - `5` (Pronoun): 代名詞 (e.g., 名詞,代名詞)
pub fn morpheme_roles(id_def_path: &Path) -> Result<Vec<u8>, DictSourceError> {
    let content = fs::read_to_string(id_def_path).map_err(DictSourceError::Io)?;
    let mut max_id: u16 = 0;
    let mut entries: Vec<(u16, u8)> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (id_str, pos_tag) = line
            .split_once(' ')
            .ok_or_else(|| DictSourceError::Parse(format!("invalid id.def line: {line}")))?;
        let id: u16 = id_str
            .parse()
            .map_err(|_| DictSourceError::Parse(format!("invalid id in id.def: {id_str}")))?;

        let fields: Vec<&str> = pos_tag.split(',').collect();
        let role = if fields.len() >= 2 {
            if fields[0] == "助動詞" || fields[0] == "助詞" {
                ROLE_FUNCTION_WORD
            } else if fields[0] == "接頭詞" {
                ROLE_PREFIX
            } else if fields[1] == "接尾" {
                ROLE_SUFFIX
            } else if fields[1] == "非自立" {
                ROLE_NON_INDEPENDENT
            } else if fields[1] == "代名詞" {
                ROLE_PRONOUN
            } else if fields.len() >= 3 && fields[2] == "人名" {
                ROLE_PERSON_NAME
            } else {
                ROLE_CONTENT_WORD
            }
        } else {
            ROLE_CONTENT_WORD
        };

        if id > max_id {
            max_id = id;
        }
        entries.push((id, role));
    }

    let mut roles = vec![ROLE_CONTENT_WORD; max_id as usize + 1];
    for (id, role) in entries {
        roles[id as usize] = role;
    }
    Ok(roles)
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
    fn test_function_word_id_range() {
        let dir = std::env::temp_dir().join("lexime_test_fw_range");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.def");
        fs::write(
            &path,
            make_id_def(&[
                "0 BOS/EOS,*,*,*,*,*,*",
                "29 助動詞,*,*,*,不変化型,基本形,*",
                "100 助動詞,*,*,*,一段,基本形,*",
                "267 助動詞,*,*,*,特殊・ダ,基本形,*",
                "268 助詞,格助詞,一般,*,*,*,*",
                "400 助詞,係助詞,*,*,*,*,*",
                "433 助詞,終助詞,*,*,*,*,*",
                "1852 名詞,一般,*,*,*,*,*",
            ]),
        )
        .unwrap();

        let (min, max) = function_word_id_range(&path).unwrap();
        assert_eq!(min, 29);
        assert_eq!(max, 433);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_function_word_id_range_empty() {
        let dir = std::env::temp_dir().join("lexime_test_fw_range_empty");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.def");
        fs::write(
            &path,
            make_id_def(&["0 BOS/EOS,*,*,*,*,*,*", "1852 名詞,一般,*,*,*,*,*"]),
        )
        .unwrap();

        let (min, max) = function_word_id_range(&path).unwrap();
        assert_eq!(min, 0);
        assert_eq!(max, 0);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_morpheme_roles() {
        let dir = std::env::temp_dir().join("lexime_test_morpheme_roles");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.def");
        fs::write(
            &path,
            make_id_def(&[
                "0 BOS/EOS,*,*,*,*,*,*",
                "29 助動詞,*,*,*,不変化型,基本形,*",
                "268 助詞,格助詞,一般,*,*,*,*",
                "433 助詞,終助詞,*,*,*,*,*",
                "1852 名詞,一般,*,*,*,*,*",
                "1860 名詞,接尾,一般,*,*,*,*",
                "1870 動詞,接尾,*,*,一段,基本形,*",
                "1880 形容詞,接尾,*,*,形容詞・アウオ段,基本形,*",
                "2600 接頭詞,名詞接続,*,*,*,*,*",
                "2641 接頭詞,数接続,*,*,*,*,*",
                "680 動詞,自立,*,*,一段,基本形,*",
                "690 名詞,非自立,一般,*,*,*,*",
                "700 動詞,非自立,*,*,一段,基本形,*",
                "1900 名詞,代名詞,一般,*,*,*,*",
                "1922 名詞,固有名詞,人名,一般,*,*,*",
                "1923 名詞,固有名詞,人名,姓,*,*,*",
                "1924 名詞,固有名詞,人名,名,*,*,*",
            ]),
        )
        .unwrap();

        let roles = morpheme_roles(&path).unwrap();
        assert_eq!(roles[0], ROLE_CONTENT_WORD); // BOS/EOS
        assert_eq!(roles[29], ROLE_FUNCTION_WORD); // 助動詞
        assert_eq!(roles[268], ROLE_FUNCTION_WORD); // 助詞
        assert_eq!(roles[433], ROLE_FUNCTION_WORD); // 助詞
        assert_eq!(roles[1852], ROLE_CONTENT_WORD); // 名詞,一般
        assert_eq!(roles[1860], ROLE_SUFFIX); // 名詞,接尾
        assert_eq!(roles[1870], ROLE_SUFFIX); // 動詞,接尾
        assert_eq!(roles[1880], ROLE_SUFFIX); // 形容詞,接尾
        assert_eq!(roles[2600], ROLE_PREFIX); // 接頭詞
        assert_eq!(roles[2641], ROLE_PREFIX); // 接頭詞
        assert_eq!(roles[680], ROLE_CONTENT_WORD); // 動詞,自立
        assert_eq!(roles[690], ROLE_NON_INDEPENDENT); // 名詞,非自立
        assert_eq!(roles[700], ROLE_NON_INDEPENDENT); // 動詞,非自立
        assert_eq!(roles[1900], ROLE_PRONOUN); // 名詞,代名詞
        assert_eq!(roles[1922], ROLE_PERSON_NAME); // 名詞,固有名詞,人名,一般
        assert_eq!(roles[1923], ROLE_PERSON_NAME); // 名詞,固有名詞,人名,姓
        assert_eq!(roles[1924], ROLE_PERSON_NAME); // 名詞,固有名詞,人名,名

        fs::remove_dir_all(&dir).ok();
    }
}
