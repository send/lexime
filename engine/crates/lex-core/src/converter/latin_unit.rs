//! Latin units: a counter (助数詞) written in Latin letters only (`s`, `μs`,
//! `kB`, `ft`), which stands only right after a digit the user typed.
//!
//! The rule is POS (counter) × script (pure Latin surface, typed ASCII
//! digit), not a list of words. Two surface flags are computed once per
//! segment — by the lattice when it pushes a node, and from a
//! `RichSegment`'s strings where a path is all there is — and every
//! consumer asks the same two predicates:
//!
//! - [`is_latin_unit`]: the segment is a Latin unit (needs the connection
//!   matrix for the counter role; with none, nothing is one).
//! - [`latin_unit_admissible`]: the segment may follow its predecessor (a
//!   Latin unit only after a typed digit).
//!
//! Viterbi (BOS initialisation and the forward pass) and resegment's two new
//! adjacencies form paths through [`latin_unit_admissible`]: a Latin unit is never
//! reached from anything but a typed digit (no transition, not a price).
//! Rerank prices a formed Latin unit as katakana and an unformed one as
//! Latin (`cost::segment_script`); NumericRewriter pairs a Latin unit with
//! half-width digits only.

use crate::dict::connection::ConnectionMatrix;
use crate::unicode::{is_hiragana, is_kanji, is_katakana, is_latin};

use super::viterbi::RichSegment;

/// The surface has an ASCII letter (the `latin_penalty` trigger) and no
/// Japanese character (hiragana, katakana, kanji): `s`, `μs`, `°C`, `mol%`,
/// not `%` (no letter) nor `km先` / `kバイト` (mixed). Empty is false.
pub fn pure_latin(surface: &str) -> bool {
    let mut has_latin = false;
    for c in surface.chars() {
        if is_hiragana(c) || is_katakana(c) || is_kanji(c) {
            return false;
        }
        has_latin |= is_latin(c);
    }
    has_latin
}

/// A digit the user typed: reading = surface = one ASCII digit (the lattice's
/// unknown fallback for a typed `3`). Dictionary readings are kana, so a
/// dictionary `さん`→`3` is not one.
pub(crate) fn typed_digit(reading: &str, surface: &str) -> bool {
    let mut chars = surface.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_digit())
        && reading == surface
}

/// A Latin unit: a pure-Latin surface on a counter POS. The one place that
/// handles a missing connection matrix — with none, nothing is a Latin unit
/// and every rule built on it falls back to the plain Latin behaviour.
pub(crate) fn is_latin_unit(
    pure_latin: bool,
    left_id: u16,
    conn: Option<&ConnectionMatrix>,
) -> bool {
    pure_latin && conn.is_some_and(|c| c.is_counter(left_id))
}

/// A segment may follow its predecessor: it is not a Latin unit, or the
/// predecessor is a typed digit (BOS is not one).
pub(crate) fn latin_unit_admissible(prev_typed_digit: bool, cur_latin_unit: bool) -> bool {
    !cur_latin_unit || prev_typed_digit
}

/// [`is_latin_unit`] of a segment that is not a lattice node index (a path's
/// segment): the flag from its surface by the function the lattice uses.
pub(crate) fn segment_is_latin_unit(seg: &RichSegment, conn: Option<&ConnectionMatrix>) -> bool {
    is_latin_unit(pure_latin(&seg.surface), seg.left_id, conn)
}

/// [`typed_digit`] of a path's segment.
pub(crate) fn segment_typed_digit(seg: &RichSegment) -> bool {
    typed_digit(&seg.reading, &seg.surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_latin_needs_a_letter_and_no_japanese() {
        for s in [
            "s", "μs", "kB", "°C", "K", "bit", "ft", "ppm", "mol%", "%OFF", "n",
        ] {
            assert!(pure_latin(s), "{s}");
        }
        for s in [
            "%",
            "Ω",
            "℃",
            "3",
            "３",
            "μ",
            "km先",
            "kバイト",
            "GB超え",
            "秒",
            "",
        ] {
            assert!(!pure_latin(s), "{s}");
        }
    }

    #[test]
    fn typed_digit_is_one_ascii_digit_read_as_itself() {
        assert!(typed_digit("3", "3"));
        assert!(typed_digit("0", "0"));
        assert!(!typed_digit("さん", "3"));
        assert!(!typed_digit("に", "2"));
        assert!(!typed_digit("３", "３"));
        assert!(!typed_digit("12", "12"));
        assert!(!typed_digit("n", "n"));
        assert!(!typed_digit("", ""));
    }

    #[test]
    fn latin_unit_admissible_only_after_a_typed_digit() {
        assert!(latin_unit_admissible(true, true));
        assert!(!latin_unit_admissible(false, true));
        assert!(latin_unit_admissible(false, false));
        assert!(latin_unit_admissible(true, false));
    }
}
