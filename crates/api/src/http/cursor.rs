use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::ApiError;

const PREFIX: &str = "entry:";

pub fn encode(entry_id: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{PREFIX}{entry_id}"))
}

pub fn decode(cursor: &str) -> Result<i64, ApiError> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.strip_prefix(PREFIX)?.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| ApiError::BadRequest("invalid cursor".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for id in [1, 42, i64::MAX] {
            assert_eq!(decode(&encode(id)).unwrap(), id);
        }
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "!!!",
            "MTIz",
            &URL_SAFE_NO_PAD.encode("entry:-5"),
            &URL_SAFE_NO_PAD.encode("entry:x"),
        ] {
            assert!(decode(bad).is_err(), "{bad} should be rejected");
        }
    }
}
