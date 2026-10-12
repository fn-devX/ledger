use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use uuid::Uuid;

pub const STREAM: &str = "LEDGER";
pub const STREAM_SUBJECTS: &str = "ledger.>";

pub trait Event: Serialize + DeserializeOwned {
    const TYPE: &'static str;
    const VERSION: u16;
    const SUBJECT: &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub event_type: String,
    pub version: u16,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub data: T,
}

impl<T: Event> Envelope<T> {
    pub fn new(id: Uuid, occurred_at: OffsetDateTime, data: T) -> Self {
        Self {
            id,
            event_type: T::TYPE.to_owned(),
            version: T::VERSION,
            occurred_at,
            data,
        }
    }

    pub fn subject(&self) -> &'static str {
        T::SUBJECT
    }

    pub fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEnvelope {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub event_type: String,
    pub version: u16,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub data: serde_json::Value,
}

impl RawEnvelope {
    pub fn parse(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }

    pub fn is<T: Event>(&self) -> bool {
        self.event_type == T::TYPE && self.version == T::VERSION
    }

    pub fn decode<T: Event>(self) -> serde_json::Result<Envelope<T>> {
        Ok(Envelope {
            id: self.id,
            event_type: self.event_type,
            version: self.version,
            occurred_at: self.occurred_at,
            data: serde_json::from_value(self.data)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Party {
    pub account_id: Uuid,
    pub holder_id: Option<Uuid>,
    pub owner: String,
    pub balance_after_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPosted {
    pub transfer_id: Uuid,
    pub kind: String,
    pub amount_minor: i64,
    pub currency: String,
    pub initiated_by: Option<Uuid>,
    pub from: Party,
    pub to: Party,
}

impl Event for TransferPosted {
    const TYPE: &'static str = "transfer.posted";
    const VERSION: u16 = 1;
    const SUBJECT: &'static str = "ledger.transfer.posted";
}

#[cfg(feature = "jetstream")]
pub mod jetstream {
    use std::time::Duration;

    use async_nats::jetstream::{Context, stream};

    pub async fn ensure_stream(js: &Context) -> Result<stream::Stream, async_nats::Error> {
        let config = stream::Config {
            name: super::STREAM.to_owned(),
            subjects: vec![super::STREAM_SUBJECTS.to_owned()],
            duplicate_window: Duration::from_secs(10 * 60),
            max_age: Duration::from_secs(30 * 24 * 60 * 60),
            storage: stream::StorageType::File,
            ..Default::default()
        };
        Ok(js.get_or_create_stream(config).await?)
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn sample() -> Envelope<TransferPosted> {
        let party = |n: u128, owner: &str, balance| Party {
            account_id: Uuid::from_u128(n),
            holder_id: Some(Uuid::from_u128(n + 100)),
            owner: owner.into(),
            balance_after_minor: balance,
        };
        Envelope::new(
            Uuid::from_u128(1),
            datetime!(2026-10-12 10:00 UTC),
            TransferPosted {
                transfer_id: Uuid::from_u128(2),
                kind: "transfer".into(),
                amount_minor: 25_000,
                currency: "TJS".into(),
                initiated_by: Some(Uuid::from_u128(103)),
                from: party(3, "Firuz", 75_000),
                to: party(4, "Nigina", 25_000),
            },
        )
    }

    #[test]
    fn wire_format_is_stable() {
        let json: serde_json::Value = serde_json::from_slice(&sample().to_json().unwrap()).unwrap();
        assert_eq!(json["type"], "transfer.posted");
        assert_eq!(json["version"], 1);
        assert_eq!(json["occurred_at"], "2026-10-12T10:00:00Z");
        assert_eq!(json["data"]["from"]["owner"], "Firuz");
        assert_eq!(json["data"]["to"]["balance_after_minor"], 25_000);
    }

    #[test]
    fn raw_envelope_dispatch() {
        let raw = RawEnvelope::parse(&sample().to_json().unwrap()).unwrap();
        assert!(raw.is::<TransferPosted>());
        assert_eq!(raw.decode::<TransferPosted>().unwrap(), sample());
    }
}
