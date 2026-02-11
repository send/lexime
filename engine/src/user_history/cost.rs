use crate::converter::cost::{conn_cost, script_cost, CostFunction, SEGMENT_PENALTY};
use crate::converter::LatticeNode;
use crate::dict::connection::ConnectionMatrix;

use super::UserHistory;

/// Cost function that incorporates user history for adaptive ranking.
pub struct LearnedCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
    history: &'a UserHistory,
}

impl<'a> LearnedCostFunction<'a> {
    pub fn new(conn: Option<&'a ConnectionMatrix>, history: &'a UserHistory) -> Self {
        Self { conn, history }
    }
}

impl CostFunction for LearnedCostFunction<'_> {
    fn word_cost(&self, node: &LatticeNode) -> i64 {
        node.cost as i64 + SEGMENT_PENALTY + script_cost(&node.surface)
            - self.history.unigram_boost(&node.reading, &node.surface)
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
        let default_cost_fn = LearnedCostFunction::new(None, &history);
        let result = convert_with_cost(&dict, &default_cost_fn, "きょう");
        assert_eq!(result[0].surface, "今日");

        // Record "京" many times to overcome the cost difference
        for _ in 0..10 {
            history.record(&[("きょう".into(), "京".into())]);
        }

        let learned_cost_fn = LearnedCostFunction::new(None, &history);
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

        let learned_cost_fn = LearnedCostFunction::new(None, &history);
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
    fn test_viterbi_with_history() {
        let dict = test_dict();
        let mut history = UserHistory::new();

        // "京" (cost=5000) normally loses to "今日" (cost=3000)
        // Record "京" enough to overcome the 2000 cost difference
        for _ in 0..10 {
            history.record(&[("きょう".into(), "京".into())]);
        }

        let cost_fn = LearnedCostFunction::new(None, &history);
        let result = convert_with_cost(&dict, &cost_fn, "きょう");
        assert_eq!(result[0].surface, "京");
    }
}
