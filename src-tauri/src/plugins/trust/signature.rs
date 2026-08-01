#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use jarvis_plugin_protocol::catalog::SignedCatalog;
    use serde_json::json;

    use super::{catalog_digest, catalog_signature_message, verify_package_signature};

    const PUBLIC_KEY: &str =
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const SIGNATURE: &str =
        "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ==";
    const PACKAGE_MESSAGE: &[u8] = b"jarvis-plugin-package-v1\0{}";

    fn minimal_catalog() -> SignedCatalog {
        SignedCatalog::parse(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "sequence": 1,
                "issuedAt": "2026-08-01T00:00:00Z",
                "expiresAt": "2026-08-02T00:00:00Z",
                "previousDigest": null,
                "payload": {
                    "publisherLineages": [],
                    "releases": [],
                    "rootRotation": null,
                    "revokedPackageDigests": [],
                    "revokedPublisherKeys": []
                },
                "signatures": [{
                    "algorithm": "ed25519",
                    "keyId": "jarvis.root:1",
                    "value": SIGNATURE
                }]
            }))
            .unwrap(),
        )
        .unwrap()
    }

    fn decode_public_key(value: &str) -> [u8; 32] {
        let mut output = [0u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            output[index] = (nibble(chunk[0]) << 4) | nibble(chunk[1]);
        }
        output
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("fixture public key is lowercase hex"),
        }
    }

    fn encode_public_key(value: &[u8; 32]) -> String {
        value.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn package_signature_known_answer_accepts_fixed_vector() {
        verify_package_signature(PUBLIC_KEY, PACKAGE_MESSAGE, SIGNATURE).unwrap();
    }

    #[test]
    fn package_signature_known_answer_rejects_one_bit_message_change() {
        let mut changed = PACKAGE_MESSAGE.to_vec();
        changed[changed.len() - 1] ^= 1;
        assert_eq!(
            verify_package_signature(PUBLIC_KEY, &changed, SIGNATURE)
                .unwrap_err()
                .code(),
            "package_signature_invalid"
        );
    }

    #[test]
    fn package_signature_known_answer_rejects_one_bit_signature_change() {
        let mut changed = STANDARD.decode(SIGNATURE).unwrap();
        changed[0] ^= 1;
        let changed = STANDARD.encode(changed);
        assert_eq!(
            verify_package_signature(PUBLIC_KEY, PACKAGE_MESSAGE, &changed)
                .unwrap_err()
                .code(),
            "package_signature_invalid"
        );
    }

    #[test]
    fn package_signature_known_answer_rejects_one_bit_public_key_change() {
        let mut changed = decode_public_key(PUBLIC_KEY);
        changed[0] ^= 1;
        assert_eq!(
            verify_package_signature(&encode_public_key(&changed), PACKAGE_MESSAGE, SIGNATURE)
                .unwrap_err()
                .code(),
            "package_signature_invalid"
        );
    }

    #[test]
    fn catalog_message_omits_signatures_and_uses_exact_jcs_domain() {
        let message = catalog_signature_message(&minimal_catalog()).unwrap();
        assert_eq!(
            message,
            b"jarvis-plugin-catalog-v1\0{\"expiresAt\":\"2026-08-02T00:00:00Z\",\"issuedAt\":\"2026-08-01T00:00:00Z\",\"payload\":{\"publisherLineages\":[],\"releases\":[],\"revokedPackageDigests\":[],\"revokedPublisherKeys\":[],\"rootRotation\":null},\"previousDigest\":null,\"schemaVersion\":1,\"sequence\":1}"
        );
    }

    #[test]
    fn catalog_digest_is_bound_to_the_unsigned_domain_message() {
        assert_eq!(
            catalog_digest(&minimal_catalog()).unwrap().as_str(),
            "sha256:f725b883273d682a43dea9470f2ff74f3ef843fe52f68699d9897d53d99882ae"
        );
    }
}
