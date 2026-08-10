use jarvis_power_core::protocol::{
    decode_request, decode_response, encode_request, encode_response, ErrorCode, ProtocolError,
    Request, RequestEnvelope, RequestId, Response, ResponseEnvelope, DEFAULT_TTL_MS,
    MAX_FRAME_BYTES, MAX_TTL_MS, MIN_TTL_MS, PROTOCOL_VERSION, RENEW_EVERY_MS, SERVICE_LABEL,
};

const REQUEST_ID: &str = "018f0000-0000-7000-8000-000000000001";

fn request(request: Request) -> RequestEnvelope {
    RequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        request,
    }
}

fn acquire_request(profile: &str, owner_generation: &str, ttl_ms: u64) -> RequestEnvelope {
    request(Request::AcquireLease {
        profile: profile.to_owned(),
        owner_generation: owner_generation.to_owned(),
        ttl_ms,
    })
}

#[test]
fn arbitrary_commands_and_unknown_fields_are_rejected() {
    let arbitrary_command = r#"{
        "protocolVersion": 2,
        "requestId": "018f0000-0000-7000-8000-000000000001",
        "method": "run",
        "args": ["/usr/bin/pmset", "-a", "disablesleep", "1"]
    }"#;
    assert_eq!(
        decode_request(arbitrary_command.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );

    let nested_unknown = r#"{
        "protocolVersion": 2,
        "requestId": "018f0000-0000-7000-8000-000000000001",
        "method": "acquireLease",
        "profile": "default",
        "ownerGeneration": "generation-a",
        "ttlMs": 45000,
        "security": {"allowPmset": true}
    }"#;
    assert_eq!(
        decode_request(nested_unknown.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );
}

#[test]
fn bounded_decoder_rejects_semantically_invalid_requests() {
    let invalid_ttl = format!(
        r#"{{
            "protocolVersion": 2,
            "requestId": "{REQUEST_ID}",
            "method": "acquireLease",
            "profile": "default",
            "ownerGeneration": "generation-a",
            "ttlMs": {}
        }}"#,
        MAX_TTL_MS + 1
    );
    assert_eq!(
        decode_request(invalid_ttl.as_bytes()),
        Err(ProtocolError::InvalidRequest)
    );

    let invalid_profile = format!(
        r#"{{
            "protocolVersion": 2,
            "requestId": "{REQUEST_ID}",
            "method": "acquireLease",
            "profile": "..",
            "ownerGeneration": "generation-a",
            "ttlMs": 45000
        }}"#
    );
    assert_eq!(
        decode_request(invalid_profile.as_bytes()),
        Err(ProtocolError::InvalidRequest)
    );

    let wrong_version = format!(
        r#"{{
            "protocolVersion": 1,
            "requestId": "{REQUEST_ID}",
            "method": "status"
        }}"#
    );
    assert_eq!(
        decode_request(wrong_version.as_bytes()),
        Err(ProtocolError::IncompatibleVersion)
    );
}

#[test]
fn direct_serialization_rejects_semantically_invalid_envelopes() {
    let invalid_request = acquire_request("default", "generation-a", MAX_TTL_MS + 1);
    assert!(serde_json::to_vec(&invalid_request).is_err());

    let invalid_response = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        response: Response::Released {
            lease_id: "caller-selected".to_owned(),
        },
    };
    assert!(serde_json::to_vec(&invalid_response).is_err());
}

#[test]
fn recovery_is_not_a_client_triggered_protocol_method() {
    for method in ["recoverExpired", "recover", "tick", "restoreBaseline"] {
        let input = format!(
            r#"{{
                "protocolVersion": 2,
                "requestId": "{REQUEST_ID}",
                "method": "{method}"
            }}"#
        );
        assert_eq!(
            decode_request(input.as_bytes()),
            Err(ProtocolError::MalformedFrame)
        );
    }
}

#[test]
fn wire_contract_has_no_caller_identity_or_mutation_fields() {
    let text = encode_request(&acquire_request("prod", "generation-a", 45_000))
        .expect("encode valid acquire request");
    let text = String::from_utf8(text).expect("JSON is UTF-8");

    for forbidden in [
        "command", "args", "path", "uid", "pid", "teamId", "baseline", "pmset",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into protocol: {text}"
        );
    }
}

#[test]
fn wrong_version_ttl_and_oversized_identifiers_fail_closed() {
    let wrong_version = format!(
        r#"{{
            "protocolVersion": 1,
            "requestId": "{REQUEST_ID}",
            "method": "acquireLease",
            "profile": "prod",
            "ownerGeneration": "generation-a",
            "ttlMs": 45000
        }}"#
    );
    assert_eq!(
        decode_request(wrong_version.as_bytes()),
        Err(ProtocolError::IncompatibleVersion)
    );

    assert_eq!(
        acquire_request("prod", "generation-a", MAX_TTL_MS + 1).validate(),
        Err(ProtocolError::InvalidRequest)
    );
    assert_eq!(
        acquire_request("prod", "generation-a", MIN_TTL_MS - 1).validate(),
        Err(ProtocolError::InvalidRequest)
    );
    assert_eq!(
        acquire_request(&"p".repeat(129), "generation-a", 45_000).validate(),
        Err(ProtocolError::InvalidRequest)
    );
    assert_eq!(
        acquire_request("prod", "../other-owner", 45_000).validate(),
        Err(ProtocolError::InvalidRequest)
    );
    assert_eq!(
        acquire_request("..", "generation-a", 45_000).validate(),
        Err(ProtocolError::InvalidRequest)
    );
}

#[test]
fn request_ids_must_be_canonical_uuid_v7_values() {
    for invalid in [
        "",
        "018f0000-0000-6000-8000-000000000001",
        "018f0000-0000-7000-7000-000000000001",
        "018F0000-0000-7000-8000-000000000001",
        "018f0000000070008000000000000001",
        "018f0000-0000-7000-8000-00000000000z",
    ] {
        assert!(
            RequestId::parse(invalid).is_err(),
            "{invalid:?} unexpectedly accepted"
        );
    }
}

#[test]
fn every_request_variant_round_trips_with_exact_fields() {
    let requests = [
        acquire_request("default", "generation-a", 45_000),
        request(Request::RenewLease {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            owner_generation: "generation-a".to_owned(),
            ttl_ms: 45_000,
        }),
        request(Request::ReleaseLease {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            owner_generation: "generation-a".to_owned(),
        }),
        request(Request::Status),
    ];

    for expected in requests {
        let encoded = encode_request(&expected).expect("encode valid request");
        assert_eq!(
            decode_request(&encoded).expect("decode valid request"),
            expected
        );
    }
}

#[test]
fn acquire_and_status_wire_vectors_are_stable() {
    let acquire = serde_json::to_value(acquire_request("default", "generation-a", DEFAULT_TTL_MS))
        .expect("encode acquire vector");
    assert_eq!(
        acquire,
        serde_json::json!({
            "protocolVersion": 2,
            "requestId": REQUEST_ID,
            "method": "acquireLease",
            "profile": "default",
            "ownerGeneration": "generation-a",
            "ttlMs": 45_000
        })
    );

    let status = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        response: Response::Status {
            active_leases: 2,
            mutation_active: true,
            recovery_required: false,
        },
    };
    assert_eq!(
        serde_json::to_value(status).expect("encode status vector"),
        serde_json::json!({
            "protocolVersion": 2,
            "requestId": REQUEST_ID,
            "result": "status",
            "activeLeases": 2,
            "mutationActive": true,
            "recoveryRequired": false
        })
    );

    assert_eq!(PROTOCOL_VERSION, 2);
    assert_eq!(MIN_TTL_MS, 5_000);
    assert_eq!(DEFAULT_TTL_MS, 45_000);
    assert_eq!(MAX_TTL_MS, 120_000);
    assert_eq!(RENEW_EVERY_MS, 15_000);
    assert_eq!(MAX_FRAME_BYTES, 16 * 1024);
    assert_eq!(SERVICE_LABEL, "app.jarvis.monitor.power-helper");
}

#[test]
fn duplicate_and_trailing_json_are_rejected() {
    let duplicate = format!(
        r#"{{
            "protocolVersion": 2,
            "protocolVersion": 2,
            "requestId": "{REQUEST_ID}",
            "method": "status"
        }}"#
    );
    assert_eq!(
        decode_request(duplicate.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );

    let trailing =
        format!(r#"{{"protocolVersion":2,"requestId":"{REQUEST_ID}","method":"status"}} true"#);
    assert_eq!(
        decode_request(trailing.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );
}

#[test]
fn frame_size_is_bounded_before_json_decode() {
    let oversized = vec![b' '; MAX_FRAME_BYTES + 1];
    assert_eq!(decode_request(oversized), Err(ProtocolError::FrameTooLarge));
}

#[test]
fn response_surface_is_closed_and_error_codes_are_finite() {
    let responses = [
        Response::Acquired {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            granted_ttl_ms: 45_000,
        },
        Response::Renewed {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            granted_ttl_ms: 60_000,
        },
        Response::Released {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
        },
        Response::Status {
            active_leases: 1,
            mutation_active: true,
            recovery_required: false,
        },
        Response::Error {
            code: ErrorCode::OwnerMismatch,
        },
    ];

    for response in responses {
        let envelope = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
            response,
        };
        let encoded = encode_response(&envelope).expect("encode response");
        let decoded = decode_response(&encoded).expect("decode response");
        assert_eq!(decoded, envelope);
    }

    let acquired = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        response: Response::Acquired {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            granted_ttl_ms: 45_000,
        },
    };
    let acquired_wire = encode_response(&acquired).expect("encode acquired response");
    let acquired_wire = String::from_utf8(acquired_wire).expect("JSON is UTF-8");
    assert!(!acquired_wire.contains("expiresAtMs"));
    assert!(acquired_wire.contains(r#""grantedTtlMs":45000"#));

    let free_form_error = format!(
        r#"{{
            "protocolVersion": 2,
            "requestId": "{REQUEST_ID}",
            "result": "error",
            "code": "ownerMismatch",
            "message": "please run arbitrary recovery"
        }}"#
    );
    assert_eq!(
        decode_response(free_form_error.as_bytes()),
        Err(ProtocolError::MalformedFrame)
    );
}

#[test]
fn response_validation_uses_response_errors_and_checks_version() {
    let invalid_lease = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        response: Response::Released {
            lease_id: "../caller-selected".to_owned(),
        },
    };
    assert_eq!(
        encode_response(&invalid_lease),
        Err(ProtocolError::InvalidResponse)
    );

    let invalid_expiry = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(REQUEST_ID).expect("fixture request id"),
        response: Response::Acquired {
            lease_id: "0123456789abcdef0123456789abcdef".to_owned(),
            granted_ttl_ms: 0,
        },
    };
    assert_eq!(
        encode_response(&invalid_expiry),
        Err(ProtocolError::InvalidResponse)
    );

    let encoded = format!(
        r#"{{
            "protocolVersion": 1,
            "requestId": "{REQUEST_ID}",
            "result": "error",
            "code": "incompatibleVersion"
        }}"#
    );
    assert_eq!(
        decode_response(encoded.as_bytes()),
        Err(ProtocolError::IncompatibleVersion)
    );
}
