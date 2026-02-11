use crate::dict::connection::ConnectionMatrix;

use super::lattice::LatticeNode;

/// Trait for scoring lattice paths during Viterbi search.
pub trait CostFunction: Send + Sync {
    fn word_cost(&self, node: &LatticeNode) -> i64;
    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64;
    fn bos_cost(&self, node: &LatticeNode) -> i64;
    fn eos_cost(&self, node: &LatticeNode) -> i64;
}

/// Default cost function using word costs and optional connection matrix.
pub struct DefaultCostFunction<'a> {
    conn: Option<&'a ConnectionMatrix>,
}

impl<'a> DefaultCostFunction<'a> {
    pub fn new(conn: Option<&'a ConnectionMatrix>) -> Self {
        Self { conn }
    }
}

impl CostFunction for DefaultCostFunction<'_> {
    fn word_cost(&self, node: &LatticeNode) -> i64 {
        node.cost as i64
    }

    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64 {
        self.conn
            .map(|c| c.cost(prev.right_id, next.left_id) as i64)
            .unwrap_or(0)
    }

    fn bos_cost(&self, node: &LatticeNode) -> i64 {
        self.conn
            .map(|c| c.cost(0, node.left_id) as i64)
            .unwrap_or(0)
    }

    fn eos_cost(&self, node: &LatticeNode) -> i64 {
        self.conn
            .map(|c| c.cost(node.right_id, 0) as i64)
            .unwrap_or(0)
    }
}
