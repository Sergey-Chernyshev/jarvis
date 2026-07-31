use jarvis_plugin_protocol::operation::OperationRef;

#[test]
fn operation_ref_is_an_opaque_bounded_string() {
    let operation_ref: OperationRef = serde_json::from_str(r#""runtime-operation/01""#).unwrap();
    assert_eq!(operation_ref.as_str(), "runtime-operation/01");
    assert_eq!(
        serde_json::to_string(&operation_ref).unwrap(),
        r#""runtime-operation/01""#
    );

    for invalid in ["", " operation/01", "operation\n01"] {
        assert!(
            serde_json::from_value::<OperationRef>(serde_json::json!(invalid)).is_err(),
            "accepted invalid operation ref {invalid:?}"
        );
    }
    assert!(serde_json::from_value::<OperationRef>(serde_json::json!("x".repeat(129))).is_err());
    assert!(serde_json::from_value::<OperationRef>(serde_json::json!({
        "id": "runtime-operation/01",
        "provider": "forged"
    }))
    .is_err());
}
