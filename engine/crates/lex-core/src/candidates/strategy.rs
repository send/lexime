use crate::dict::connection::ConnectionMatrix;
use crate::dict::Dictionary;
use crate::user_history::UserHistory;

use super::CandidateResponse;

/// Strategy for candidate generation.
///
/// `Standard` and `Predictive` are stateless.  `Neural` carries a mutable
/// reference to the scorer plus the preceding context string, because
/// speculative decoding needs both.
pub enum CandidateStrategy<'a> {
    Standard,
    Predictive,
    #[cfg(feature = "neural")]
    Neural {
        scorer: &'a mut crate::neural::NeuralScorer,
        context: &'a str,
    },
    /// Marker so the lifetime is always used even without the `neural` feature.
    #[cfg(not(feature = "neural"))]
    #[doc(hidden)]
    _Phantom(std::marker::PhantomData<&'a ()>),
}

impl CandidateStrategy<'_> {
    /// Dispatch tag for FFI (matches Swift `CandidateDispatch` raw values).
    ///   0 = Standard, 1 = Predictive, 2 = Neural
    pub fn dispatch_tag(&self) -> u8 {
        match self {
            Self::Standard => 0,
            Self::Predictive => 1,
            #[cfg(feature = "neural")]
            Self::Neural { .. } => 2,
            #[cfg(not(feature = "neural"))]
            Self::_Phantom(_) => unreachable!(),
        }
    }

    /// Generate candidates using the selected strategy.
    pub fn generate(
        &mut self,
        dict: &dyn Dictionary,
        conn: Option<&ConnectionMatrix>,
        history: Option<&UserHistory>,
        reading: &str,
        max_results: usize,
    ) -> CandidateResponse {
        match self {
            Self::Standard => super::standard::generate(dict, conn, history, reading, max_results),
            Self::Predictive => {
                super::predictive::generate(dict, conn, history, reading, max_results)
            }
            #[cfg(feature = "neural")]
            Self::Neural { scorer, context } => {
                super::neural::generate(scorer, dict, conn, history, context, reading, max_results)
            }
            #[cfg(not(feature = "neural"))]
            Self::_Phantom(_) => unreachable!(),
        }
    }
}
