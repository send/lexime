use crate::dict::connection::ConnectionMatrix;
use crate::settings::{settings, CostSettings};
use crate::unicode::{is_hiragana, is_kanji, is_katakana, is_latin};

use super::latin_unit::{latin_unit_admissible, segment_is_latin_unit, segment_typed_digit};
use super::lattice::Lattice;
use super::viterbi::RichSegment;

/// Cost adjustment based on the surface script.
/// - Mixed-script (kanji+kana, e.g. 通っ, 食べる): bonus (negative)
/// - Pure kanji (e.g. 方, 気, 人): small bonus (negative)
/// - Contains Latin/ASCII (e.g. death, tie, thai): heavy penalty
/// - All-katakana (e.g. タラ, オッ): penalty (positive)
/// - Otherwise (pure hiragana, etc.): no adjustment
fn script_cost(cs: &CostSettings, surface: &str, reading_chars: usize) -> i64 {
    let mut has_kanji = false;
    let mut has_kana = false;
    let mut all_katakana = !surface.is_empty();
    for c in surface.chars() {
        if is_latin(c) {
            return cs.latin_penalty;
        }
        if is_kanji(c) {
            has_kanji = true;
        }
        if is_hiragana(c) || is_katakana(c) {
            has_kana = true;
        }
        if !is_katakana(c) {
            all_katakana = false;
        }
    }
    let scale = reading_chars.min(2) as i64;
    if has_kanji && has_kana {
        -cs.mixed_script_bonus * scale / 3
    } else if has_kanji {
        -cs.pure_kanji_bonus * scale / 3
    } else if all_katakana {
        cs.katakana_penalty
    } else {
        0
    }
}

/// One segment's script price on a path (see [`segment_script`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentScript {
    pub cost: i64,
    /// A Latin unit that does not follow a typed digit: a path the
    /// formation rule should not have made, priced as plain Latin
    /// (fail-safe). Counted so a measurement can confirm there are none.
    pub latin_unit_unformed: bool,
}

/// The script price of `seg` following `prev` (`None` = BOS): the one
/// per-segment caller of [`script_cost`].
///
/// A Latin unit (`latin_unit::is_latin_unit`) formed after a typed digit
/// is priced as katakana (`katakana_penalty`, the other foreign script) —
/// the largest script price a non-Latin segment can get under any valid
/// setting, so a formed unit is never cheaper by script than its Japanese
/// name. One that is not formed (the rule's makers leaked) keeps
/// `latin_penalty`. Every other segment is [`script_cost`].
pub(crate) fn segment_script(
    seg: &RichSegment,
    prev: Option<&RichSegment>,
    conn: Option<&ConnectionMatrix>,
) -> SegmentScript {
    segment_script_in(&settings().cost, seg, prev, conn)
}

/// [`segment_script`] under the cost settings `c`.
pub(crate) fn segment_script_in(
    c: &CostSettings,
    seg: &RichSegment,
    prev: Option<&RichSegment>,
    conn: Option<&ConnectionMatrix>,
) -> SegmentScript {
    if !segment_is_latin_unit(seg, conn) {
        return SegmentScript {
            cost: script_cost(c, &seg.surface, seg.reading.chars().count()),
            latin_unit_unformed: false,
        };
    }
    if latin_unit_admissible(prev.is_some_and(segment_typed_digit), true) {
        SegmentScript {
            cost: c.katakana_penalty,
            latin_unit_unformed: false,
        }
    } else {
        SegmentScript {
            cost: c.latin_penalty,
            latin_unit_unformed: true,
        }
    }
}

/// Trait for scoring lattice paths during Viterbi search.
///
/// `DefaultCostFunction` is the one implementation, and its `word_cost` reads
/// only the node's cost and `left_id`; the `(&Lattice, usize)` signature is
/// left from a removed prefix-constrained cost that looked at the whole node.
/// The other three methods take raw IDs because `transition_cost` is the
/// most frequent call (O(P*Q) per position).
pub(crate) trait CostFunction: Send + Sync {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64;
    fn transition_cost(&self, prev_right_id: u16, next_left_id: u16) -> i64;
    fn bos_cost(&self, left_id: u16) -> i64;
    fn eos_cost(&self, right_id: u16) -> i64;
    /// The connection matrix the path-formation rule reads POS roles from
    /// (`latin_unit::node_admissible`). `None` (no matrix) leaves the rule
    /// off.
    fn conn(&self) -> Option<&ConnectionMatrix> {
        None
    }
}

/// Look up connection cost between two IDs, returning 0 if no matrix is provided.
pub fn conn_cost(conn: Option<&ConnectionMatrix>, left: u16, right: u16) -> i64 {
    conn.map(|c| c.cost(left, right) as i64).unwrap_or(0)
}

/// Default cost function using word costs and optional connection matrix.
pub(crate) struct DefaultCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
}

impl<'a> DefaultCostFunction<'a> {
    pub fn new(conn: Option<&'a ConnectionMatrix>) -> Self {
        Self { conn }
    }
}

/// Cost of one node on a path: its dictionary cost plus the segment
/// penalty (halved for function words). The single definition Viterbi and
/// [`score_path`] both use.
pub(crate) fn node_cost(word_cost: i16, left_id: u16, conn: Option<&ConnectionMatrix>) -> i64 {
    let (word, penalty) = node_terms(word_cost, left_id, conn);
    word + penalty
}

/// The two terms of [`node_cost`]: the dictionary cost and the segment
/// penalty (halved for function words).
pub(crate) fn node_terms(
    word_cost: i16,
    left_id: u16,
    conn: Option<&ConnectionMatrix>,
) -> (i64, i64) {
    let seg_penalty = settings().cost.segment_penalty;
    let is_fw = conn.is_some_and(|c| c.is_function_word(left_id));
    let penalty = if is_fw { seg_penalty / 2 } else { seg_penalty };
    (word_cost as i64, penalty)
}

/// What one segment adds to [`score_path`]: its node's two terms and the
/// transition into it (from BOS for the first segment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentTerms {
    pub word_cost: i64,
    pub segment_penalty: i64,
    pub transition_in: i64,
}

/// The per-segment terms of [`score_path`], in path order. With
/// [`eos_term`] they are everything it sums.
pub(crate) fn segment_terms<'a>(
    segments: &'a [RichSegment],
    conn: Option<&'a ConnectionMatrix>,
) -> impl Iterator<Item = SegmentTerms> + 'a {
    segments.iter().enumerate().map(move |(i, s)| {
        let (word_cost, segment_penalty) = node_terms(s.word_cost, s.left_id, conn);
        let prev_right = i.checked_sub(1).map_or(0, |j| segments[j].right_id);
        SegmentTerms {
            word_cost,
            segment_penalty,
            transition_in: conn_cost(conn, prev_right, s.left_id),
        }
    })
}

/// The transition from the last segment to EOS (0 for an empty path).
pub(crate) fn eos_term(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    segments
        .last()
        .map_or(0, |last| conn_cost(conn, last.right_id, 0))
}

/// Price a segment sequence exactly as `DefaultCostFunction` prices the same
/// path in Viterbi: node costs + BOS + transitions + EOS.
pub(crate) fn score_path(segments: &[RichSegment], conn: Option<&ConnectionMatrix>) -> i64 {
    segment_terms(segments, conn)
        .map(|t| t.word_cost + t.segment_penalty + t.transition_in)
        .sum::<i64>()
        + eos_term(segments, conn)
}

impl CostFunction for DefaultCostFunction<'_> {
    fn word_cost(&self, lattice: &Lattice, idx: usize) -> i64 {
        node_cost(lattice.cost(idx), lattice.left_id(idx), self.conn)
    }

    fn transition_cost(&self, prev_right_id: u16, next_left_id: u16) -> i64 {
        conn_cost(self.conn, prev_right_id, next_left_id)
    }

    fn bos_cost(&self, left_id: u16) -> i64 {
        conn_cost(self.conn, 0, left_id)
    }

    fn eos_cost(&self, right_id: u16) -> i64 {
        conn_cost(self.conn, right_id, 0)
    }

    fn conn(&self) -> Option<&ConnectionMatrix> {
        self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::testutil::zero_conn_with_roles;
    use crate::settings::{default_toml, parse_settings_toml};

    const COUNTER: u16 = 10;
    const NUMERAL: u16 = 20;

    /// The embedded settings with `[cost]` keys replaced (validated).
    fn cost_with(changes: &[(&str, i64)]) -> CostSettings {
        let mut toml = default_toml().to_string();
        for (key, value) in changes {
            let line = toml
                .lines()
                .find(|l| l.starts_with(&format!("{key} =")))
                .unwrap_or_else(|| panic!("{key} not in the default settings"))
                .to_string();
            toml = toml.replace(&line, &format!("{key} = {value}"));
        }
        parse_settings_toml(&toml).expect("valid settings").cost
    }

    fn seg(reading: &str, surface: &str, id: u16, cost: i16) -> RichSegment {
        RichSegment {
            reading: reading.into(),
            surface: surface.into(),
            left_id: id,
            right_id: id,
            word_cost: cost,
        }
    }

    fn conn() -> ConnectionMatrix {
        let mut roles = vec![0u8; 30];
        roles[COUNTER as usize] = 7;
        zero_conn_with_roles(30, roles)
    }

    /// Settings validate accepts with the script terms at their edges.
    fn settings_table() -> Vec<CostSettings> {
        vec![
            cost_with(&[]),
            cost_with(&[("katakana_penalty", 0)]),
            cost_with(&[("mixed_script_bonus", 0)]),
        ]
    }

    /// The order a formed Latin unit relies on (I3): no non-Latin surface
    /// gets a script price above `katakana_penalty`, under any setting
    /// validate accepts.
    #[test]
    fn non_latin_script_price_is_at_most_katakana_penalty() {
        for c in settings_table() {
            // kanji+kana, kanji only, all katakana, other (hiragana, symbol).
            for surface in ["食べる", "秒", "バイト", "の", "%", "Ω"] {
                for reading_chars in [1, 2, 3] {
                    let price = script_cost(&c, surface, reading_chars);
                    assert!(
                        price <= c.katakana_penalty,
                        "{surface} ({reading_chars}): {price} > {}",
                        c.katakana_penalty
                    );
                }
            }
        }
    }

    #[test]
    fn a_formed_latin_unit_is_priced_as_katakana() {
        let conn = conn();
        let digit = seg("3", "3", 0, 10000);
        let s = seg("びょう", "s", COUNTER, 100);
        for c in [
            cost_with(&[]),
            cost_with(&[("katakana_penalty", 2000), ("latin_penalty", 10000)]),
        ] {
            assert_eq!(
                segment_script_in(&c, &s, Some(&digit), Some(&conn)),
                SegmentScript {
                    cost: c.katakana_penalty,
                    latin_unit_unformed: false
                }
            );
            // Not formed (after a kanji numeral, or from BOS): the
            // fail-safe price, counted.
            for prev in [Some(seg("さん", "三", NUMERAL, 500)), None] {
                assert_eq!(
                    segment_script_in(&c, &s, prev.as_ref(), Some(&conn)),
                    SegmentScript {
                        cost: c.latin_penalty,
                        latin_unit_unformed: true
                    }
                );
            }
            // Without a matrix nothing is a Latin unit: plain Latin.
            assert_eq!(
                segment_script_in(&c, &s, Some(&digit), None),
                SegmentScript {
                    cost: c.latin_penalty,
                    latin_unit_unformed: false
                }
            );
            // A typed digit itself is not Latin (ASCII digits are no letters).
            assert_eq!(segment_script_in(&c, &digit, None, Some(&conn)).cost, 0);
        }
    }

    /// A unit and its Japanese name on the same id, the unit's cost one
    /// above the name's: the name comes first under every setting.
    #[test]
    fn a_formed_unit_follows_its_japanese_name() {
        let conn = conn();
        let digit = seg("3", "3", 0, 10000);
        let price = |c: &CostSettings, unit: &RichSegment| {
            let path = [digit.clone(), unit.clone()];
            score_path(&path, Some(&conn))
                + segment_script_in(c, unit, Some(&digit), Some(&conn)).cost
                + segment_script_in(c, &digit, None, Some(&conn)).cost
        };
        let mut table = settings_table();
        table.push(cost_with(&[
            ("katakana_penalty", 2000),
            ("latin_penalty", 10000),
        ]));
        for c in table {
            for (reading, name, unit) in [
                ("ばいと", "バイト", "B"),
                ("まいくろびょう", "マイクロ秒", "μs"),
            ] {
                let named = price(&c, &seg(reading, name, COUNTER, 3000));
                let latin = price(&c, &seg(reading, unit, COUNTER, 3001));
                assert!(named < latin, "{name} {named} vs {unit} {latin}");
            }
        }
    }
}
