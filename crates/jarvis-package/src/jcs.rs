use jarvis_plugin_protocol::json::{parse_bounded_json_with_limits, JsonLimits};
use serde_json::Value;

// Step A3.3 wires this bounded canonical reader into package metadata construction.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JcsError {
    InvalidJson,
    NotCanonical,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_exact_jcs(bytes: &[u8], limits: JsonLimits) -> Result<Value, JcsError> {
    let value = parse_bounded_json_with_limits(bytes, limits).map_err(|_| JcsError::InvalidJson)?;
    let canonical = serde_json_canonicalizer::to_vec(&value).map_err(|_| JcsError::InvalidJson)?;
    if canonical != bytes {
        return Err(JcsError::NotCanonical);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{parse_exact_jcs, JcsError};
    use jarvis_plugin_protocol::json::JsonLimits;
    use serde_json::json;

    fn package_limits() -> JsonLimits {
        JsonLimits {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 64,
            max_nodes: 250_000,
            max_string_bytes: 64 * 1024,
        }
    }

    #[test]
    fn rfc8785_number_formatting_is_byte_exact() {
        let raw =
            br#"{"numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001]}"#;
        let canonical = br#"{"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27]}"#;

        assert_eq!(
            parse_exact_jcs(raw.as_slice(), package_limits()).unwrap_err(),
            JcsError::NotCanonical
        );
        assert_eq!(
            parse_exact_jcs(canonical.as_slice(), package_limits()).unwrap(),
            json!({
                "numbers": [333333333.3333333_f64, 1e30_f64, 4.5_f64, 0.002_f64, 1e-27_f64]
            })
        );
    }

    #[test]
    fn rfc8785_string_escaping_is_byte_exact() {
        let canonical = b"{\"s\":\"\\u000f\\nA\\\\\\\"/\"}";
        assert!(parse_exact_jcs(canonical.as_slice(), package_limits()).is_ok());

        for noncanonical in [
            b"{\"s\":\"\\u000F\\nA\\\\\\\"/\"}".as_slice(),
            b"{\"s\":\"\\u000f\\u000aA\\\\\\\"/\"}".as_slice(),
            b"{\"s\":\"\\u000f\\nA\\\\\\\"\\/\"}".as_slice(),
        ] {
            assert_eq!(
                parse_exact_jcs(noncanonical, package_limits()).unwrap_err(),
                JcsError::NotCanonical
            );
        }
    }

    #[test]
    fn rfc8785_orders_object_names_by_utf16_code_units() {
        let canonical =
            "{\"\\r\":\"CR\",\"1\":\"one\",\"€\":\"euro\",\"😀\":\"grin\",\"דּ\":\"ligature\"}";
        assert!(parse_exact_jcs(canonical.as_bytes(), package_limits()).is_ok());

        let wrong =
            "{\"\\r\":\"CR\",\"1\":\"one\",\"€\":\"euro\",\"דּ\":\"ligature\",\"😀\":\"grin\"}";
        assert_eq!(
            parse_exact_jcs(wrong.as_bytes(), package_limits()).unwrap_err(),
            JcsError::NotCanonical
        );
    }

    #[test]
    fn exact_jcs_rejects_duplicate_bom_trailing_and_noncanonical_bytes() {
        for invalid in [
            br#"{"a":1,"a":2}"#.as_slice(),
            b"\xef\xbb\xbf{\"a\":1}".as_slice(),
            b"{\"a\":1}\n".as_slice(),
            b"{\"a\":1} ".as_slice(),
            br#"{"z":2,"a":1}"#.as_slice(),
            br#"{"a":1.0}"#.as_slice(),
        ] {
            assert!(parse_exact_jcs(invalid, package_limits()).is_err());
        }
    }

    #[test]
    fn bounded_jcs_accepts_exact_limits_and_rejects_plus_one() {
        let depth_two = JsonLimits {
            max_bytes: 5,
            max_depth: 2,
            max_nodes: 3,
            max_string_bytes: 4,
        };
        assert!(parse_exact_jcs(b"[[0]]", depth_two).is_ok());
        assert_eq!(
            parse_exact_jcs(
                b"[[0]]",
                JsonLimits {
                    max_depth: 1,
                    ..depth_two
                },
            )
            .unwrap_err(),
            JcsError::InvalidJson
        );

        let three_nodes = JsonLimits {
            max_bytes: 5,
            max_depth: 1,
            max_nodes: 3,
            max_string_bytes: 4,
        };
        assert!(parse_exact_jcs(b"[0,1]", three_nodes).is_ok());
        assert_eq!(
            parse_exact_jcs(
                b"[0,1]",
                JsonLimits {
                    max_nodes: 2,
                    ..three_nodes
                },
            )
            .unwrap_err(),
            JcsError::InvalidJson
        );

        let four_string_bytes = JsonLimits {
            max_bytes: 6,
            max_depth: 0,
            max_nodes: 1,
            max_string_bytes: 4,
        };
        assert!(parse_exact_jcs(b"\"four\"", four_string_bytes).is_ok());
        assert_eq!(
            parse_exact_jcs(
                b"\"four\"",
                JsonLimits {
                    max_string_bytes: 3,
                    ..four_string_bytes
                },
            )
            .unwrap_err(),
            JcsError::InvalidJson
        );
    }
}
