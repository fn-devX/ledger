use std::fmt;

use crate::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Currency([u8; 3]);

impl Currency {
    pub fn parse(code: &str) -> Result<Self, DomainError> {
        match code.as_bytes() {
            &[a, b, c] if [a, b, c].iter().all(u8::is_ascii_uppercase) => Ok(Self([a, b, c])),
            _ => Err(DomainError::InvalidCurrency(code.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("???")
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Money {
    minor: i64,
    currency: Currency,
}

impl Money {
    pub fn new(minor: i64, currency: Currency) -> Result<Self, DomainError> {
        if minor <= 0 {
            return Err(DomainError::NonPositiveAmount);
        }
        Ok(Self { minor, currency })
    }

    pub fn minor(&self) -> i64 {
        self.minor
    }

    pub fn currency(&self) -> Currency {
        self.currency
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:02} {}",
            self.minor / 100,
            self.minor % 100,
            self.currency
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_currency() {
        assert_eq!(Currency::parse("TJS").unwrap().as_str(), "TJS");
    }

    #[test]
    fn rejects_invalid_currency() {
        for bad in ["", "tj", "tjs", "TJSX", "T1S", "ÜSD"] {
            assert!(Currency::parse(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn money_must_be_positive() {
        let tjs = Currency::parse("TJS").unwrap();
        assert_eq!(Money::new(0, tjs), Err(DomainError::NonPositiveAmount));
        assert_eq!(Money::new(-5, tjs), Err(DomainError::NonPositiveAmount));
        assert_eq!(Money::new(12_345, tjs).unwrap().to_string(), "123.45 TJS");
    }
}
