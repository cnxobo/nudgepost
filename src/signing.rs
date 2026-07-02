use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn dingtalk_sign(timestamp_ms: i64, secret: &str) -> String {
    let string_to_sign = format!("{timestamp_ms}\n{secret}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(string_to_sign.as_bytes());
    let encoded = STANDARD.encode(mac.finalize().into_bytes());
    urlencoding::encode(&encoded).into_owned()
}

pub fn feishu_sign(timestamp_seconds: i64, secret: &str) -> String {
    let key = format!("{timestamp_seconds}\n{secret}");
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key");
    mac.update(b"");
    STANDARD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dingtalk_signature_is_stable() {
        let sign = dingtalk_sign(1700000000000, "secret");
        assert_eq!(sign, dingtalk_sign(1700000000000, "secret"));
        assert!(!sign.is_empty());
        assert!(!sign.contains('+'));
    }

    #[test]
    fn feishu_signature_is_stable() {
        let sign = feishu_sign(1700000000, "secret");
        assert_eq!(sign, feishu_sign(1700000000, "secret"));
        assert!(!sign.is_empty());
    }
}
