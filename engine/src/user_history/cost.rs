use crate::converter::cost::{conn_cost, CostFunction, SEGMENT_PENALTY};
use crate::converter::LatticeNode;
use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;

use super::UserHistory;

/// Cost function that incorporates user history for adaptive ranking.
pub struct LearnedCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
    dict: Option<&'a dyn Dictionary>,
    history: &'a UserHistory,
}

impl<'a> LearnedCostFunction<'a> {
    pub fn new(
        conn: Option<&'a ConnectionMatrix>,
        dict: Option<&'a dyn Dictionary>,
        history: &'a UserHistory,
    ) -> Self {
        Self {
            conn,
            dict,
            history,
        }
    }

    /// Check whether (reading, surface) has any dictionary entry with a
    /// function-word POS ID. This ensures that surfaces like "し" which
    /// appear as both 助動詞 (L=96) and 動詞 (L=537) are treated as
    /// function words for boost purposes.
    fn is_function_word_surface(&self, reading: &str, surface: &str) -> bool {
        let Some(conn) = self.conn else {
            return false;
        };
        let Some(dict) = self.dict else {
            return conn.is_function_word(0); // unreachable in practice
        };
        let Some(entries) = dict.lookup(reading) else {
            return false;
        };
        entries
            .iter()
            .any(|e| e.surface == surface && conn.is_function_word(e.left_id))
    }
}

impl CostFunction for LearnedCostFunction<'_> {
    fn word_cost(&self, node: &LatticeNode) -> i64 {
        let boost = if self.is_function_word_surface(&node.reading, &node.surface) {
            0
        } else {
            self.history.unigram_boost(&node.reading, &node.surface)
        };
        node.cost as i64 + SEGMENT_PENALTY - boost
    }

    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64 {
        conn_cost(self.conn, prev.right_id, next.left_id)
            - self
                .history
                .bigram_boost(&prev.surface, &next.reading, &next.surface)
    }

    fn bos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, 0, node.left_id)
    }

    fn eos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, node.right_id, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::convert_with_cost;
    use crate::converter::testutil::test_dict;

    #[test]
    fn test_learned_cost_unigram() {
        let dict = test_dict();
        let mut history = UserHistory::new();

        // Without history: "今日" has cost 3000, "京" has cost 5000
        let default_cost_fn = LearnedCostFunction::new(None, None, &history);
        let result = convert_with_cost(&dict, &default_cost_fn, None, "きょう");
        assert_eq!(result[0].surface, "今日");

        // Record "京" many times to overcome the cost difference
        for _ in 0..10 {
            history.record(&[("きょう".into(), "京".into())]);
        }

        let learned_cost_fn = LearnedCostFunction::new(None, None, &history);
        let node_kyou = LatticeNode {
            start: 0,
            end: 3,
            reading: "きょう".into(),
            surface: "今日".into(),
            cost: 3000,
            left_id: 100,
            right_id: 100,
        };
        let node_kyo = LatticeNode {
            start: 0,
            end: 3,
            reading: "きょう".into(),
            surface: "京".into(),
            cost: 5000,
            left_id: 101,
            right_id: 101,
        };
        // "京" should now have lower effective cost due to boost
        assert!(learned_cost_fn.word_cost(&node_kyo) < learned_cost_fn.word_cost(&node_kyou));
    }

    #[test]
    fn test_learned_cost_bigram() {
        let mut history = UserHistory::new();
        history.record(&[("きょう".into(), "今日".into()), ("は".into(), "は".into())]);

        let learned_cost_fn = LearnedCostFunction::new(None, None, &history);
        let prev = LatticeNode {
            start: 0,
            end: 3,
            reading: "きょう".into(),
            surface: "今日".into(),
            cost: 3000,
            left_id: 100,
            right_id: 100,
        };
        let next = LatticeNode {
            start: 3,
            end: 4,
            reading: "は".into(),
            surface: "は".into(),
            cost: 2000,
            left_id: 200,
            right_id: 200,
        };
        // Transition cost should be negative (boosted)
        assert!(learned_cost_fn.transition_cost(&prev, &next) < 0);
    }

    #[test]
    fn test_function_word_no_boost() {
        use crate::dict::{DictEntry, TrieDictionary};

        let mut history = UserHistory::new();
        // Record "し" many times
        for _ in 0..10 {
            history.record(&[("し".into(), "し".into())]);
        }

        // fw range 100..=500; "し" has both fw entry (L=300) and non-fw (L=600)
        let text = "2 2\n0\n0\n0\n0\n";
        let conn = ConnectionMatrix::from_text_with_metadata(text, 100, 500).unwrap();

        // Dict: "し"/"し" has a function-word entry (L=300, in range)
        // and a non-function-word entry (L=600, out of range)
        let dict = TrieDictionary::from_entries(
            [(
                "し".to_string(),
                vec![
                    DictEntry {
                        surface: "し".into(),
                        cost: 0,
                        left_id: 300, // in fw range
                        right_id: 300,
                    },
                    DictEntry {
                        surface: "し".into(),
                        cost: 0,
                        left_id: 600, // NOT in fw range
                        right_id: 600,
                    },
                ],
            )]
            .into_iter(),
        );

        let cost_fn = LearnedCostFunction::new(Some(&conn), Some(&dict), &history);

        // Even the non-fw node (L=600) should NOT get boost because
        // the same (reading, surface) has a fw entry in the dict
        let non_fw_node = LatticeNode {
            start: 0,
            end: 1,
            reading: "し".into(),
            surface: "し".into(),
            cost: 2000,
            left_id: 600, // outside fw range
            right_id: 600,
        };
        let expected_no_boost = 2000 + SEGMENT_PENALTY;
        assert_eq!(
            cost_fn.word_cost(&non_fw_node),
            expected_no_boost,
            "node sharing (reading, surface) with a fw entry should get no boost"
        );
    }

    #[test]
    fn test_viterbi_with_history() {
        let dict = test_dict();
        let mut history = UserHistory::new();

        // "京" (cost=5000) normally loses to "今日" (cost=3000)
        // Record "京" enough to overcome the 2000 cost difference
        for _ in 0..10 {
            history.record(&[("きょう".into(), "京".into())]);
        }

        let cost_fn = LearnedCostFunction::new(None, None, &history);
        let result = convert_with_cost(&dict, &cost_fn, None, "きょう");
        assert_eq!(result[0].surface, "京");
    }
}
