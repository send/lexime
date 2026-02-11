use crate::dict::connection::ConnectionMatrix;

use super::lattice::LatticeNode;

/// Trait for scoring lattice paths during Viterbi search.
pub trait CostFunction: Send + Sync {
    fn word_cost(&self, node: &LatticeNode) -> i64;
    fn transition_cost(&self, prev: &LatticeNode, next: &LatticeNode) -> i64;
    fn bos_cost(&self, node: &LatticeNode) -> i64;
    fn eos_cost(&self, node: &LatticeNode) -> i64;
}

/// Look up connection cost between two IDs, returning 0 if no matrix is provided.
pub fn conn_cost(conn: Option<&ConnectionMatrix>, left: u16, right: u16) -> i64 {
    conn.map(|c| c.cost(left, right) as i64).unwrap_or(0)
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
        conn_cost(self.conn, prev.right_id, next.left_id)
    }

    fn bos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, 0, node.left_id)
    }

    fn eos_cost(&self, node: &LatticeNode) -> i64 {
        conn_cost(self.conn, node.right_id, 0)
    }
}
