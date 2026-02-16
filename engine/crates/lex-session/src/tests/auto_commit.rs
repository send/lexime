use super::*;
use crate::ConversionMode;

#[test]
fn test_set_conversion_mode_ghosttext() {
    let dict = make_test_dict();
    let mut session = InputSession::new(dict.clone(), None, None);

    session.set_conversion_mode(ConversionMode::GhostText);
    assert_eq!(session.conversion_mode, ConversionMode::GhostText);
    assert_eq!(session.conversion_mode.candidate_dispatch(), 2);
}
