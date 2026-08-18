use veyra_runtime::PublicationError;

#[test]
fn publication_error_display_is_stable_for_every_variant() {
    assert_eq!(
        PublicationError::ReadyWithoutPayload.to_string(),
        "ReadyWithoutPayload"
    );
    assert_eq!(
        PublicationError::PayloadRequiresReadyPhase.to_string(),
        "PayloadRequiresReadyPhase"
    );
}
