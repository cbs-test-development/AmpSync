//! Minimal, unverified JWT payload decoding for diagnostics only.
//!
//! This never checks the signature and must never be used to make an authorization decision -
//! it exists only so the probe's report can describe a token it obtained (audience, scopes,
//! remaining lifetime) without ever recording the token itself. `decode_claims_redacted` keeps
//! exactly four things: `aud`, the `scp`/`roles` claim (scope/role NAMES and values - these are
//! not secrets), minutes until `exp`, and the token's length. Every other claim (oid, upn, tid,
//! unique_name, ...) is dropped before it ever reaches the report.

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};

fn decode_segment(seg: &str) -> Result<Value> {
    // JWTs use base64url, normally unpadded, but tolerate a padded segment too.
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = engine
        .decode(seg)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(seg))
        .map_err(|e| anyhow!("payload segment is not valid base64url: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| anyhow!("payload segment is not valid JSON: {e}"))
}

/// Decode a JWT's payload (header.payload.signature, signature untouched and unverified) and
/// return only the non-identifying claims described above, plus the raw token length.
pub fn decode_claims_redacted(token: &str) -> Result<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!("not a JWT (expected 3 dot-separated parts, got {})", parts.len()));
    }
    let payload = decode_segment(parts[1])?;
    let now = chrono::Utc::now().timestamp();
    let exp_in_minutes = payload.get("exp").and_then(|v| v.as_i64()).map(|e| ((e - now) as f64 / 60.0 * 10.0).round() / 10.0);
    Ok(json!({
        "token_length": token.len(),
        "aud": payload.get("aud").cloned().unwrap_or(Value::Null),
        "scp": payload.get("scp").cloned().unwrap_or(Value::Null),
        "roles": payload.get("roles").cloned().unwrap_or(Value::Null),
        "exp_in_minutes": exp_in_minutes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64url(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes())
    }

    #[test]
    fn decodes_and_redacts_claims() {
        let now = chrono::Utc::now().timestamp();
        let payload = json!({
            "aud": "https://contoso.sharepoint.com",
            "scp": "AllSites.Read",
            "upn": "someone@contoso.com",
            "oid": "11111111-2222-3333-4444-555555555555",
            "exp": now + 3600,
        });
        let token = format!("{}.{}.{}", b64url(r#"{"alg":"none"}"#), b64url(&payload.to_string()), "sig");
        let claims = decode_claims_redacted(&token).unwrap();
        assert_eq!(claims["aud"], "https://contoso.sharepoint.com");
        assert_eq!(claims["scp"], "AllSites.Read");
        assert_eq!(claims["token_length"], json!(token.len()));
        let mins = claims["exp_in_minutes"].as_f64().unwrap();
        assert!((mins - 60.0).abs() < 0.2, "expected ~60 minutes, got {mins}");

        // Identifying claims must never survive into the redacted output.
        let rendered = claims.to_string();
        assert!(!rendered.contains("someone@contoso.com"));
        assert!(!rendered.contains("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn missing_exp_yields_no_minutes() {
        let payload = json!({"aud": "https://contoso.sharepoint.com"});
        let token = format!("{}.{}.{}", b64url(r#"{"alg":"none"}"#), b64url(&payload.to_string()), "sig");
        let claims = decode_claims_redacted(&token).unwrap();
        assert!(claims["exp_in_minutes"].is_null());
        assert!(claims["scp"].is_null());
        assert!(claims["roles"].is_null());
    }

    #[test]
    fn rejects_non_jwt() {
        assert!(decode_claims_redacted("not-a-jwt").is_err());
        assert!(decode_claims_redacted("a.b").is_err());
        assert!(decode_claims_redacted("a.not-base64url!!.c").is_err());
    }
}
