use std::{fmt, str::FromStr};

use crate::ContractError;

/// Nonnegative provider currency in integral micro-USD; floating point is not
/// part of an authority or cost contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct MicroUsd(u64);

impl MicroUsd {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, ContractError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(ContractError::InvalidValue {
                field: "micro_usd",
                reason: "sum exceeds u64",
            })
    }

    /// Parse a nonnegative USD decimal without using floating point, rounding
    /// any sub-micro-USD remainder up so a provider charge is never
    /// understated at the Factory boundary.
    pub fn parse_decimal_usd(value: &str) -> Result<Self, ContractError> {
        let (whole, fraction) = value.split_once('.').map_or((value, None), |(whole, fraction)| {
            (whole, Some(fraction))
        });
        if whole.is_empty()
            || !whole.as_bytes().iter().all(u8::is_ascii_digit)
            || fraction.is_some_and(|fraction| {
                fraction.is_empty() || !fraction.as_bytes().iter().all(u8::is_ascii_digit)
            })
        {
            return Err(ContractError::InvalidValue {
                field: "USD decimal",
                reason: "must be an unsigned decimal with an optional fractional part",
            });
        }

        let whole = whole
            .parse::<u64>()
            .map_err(|_| ContractError::InvalidValue {
                field: "USD decimal",
                reason: "exceeds u64 micro-USD",
            })?;
        let fraction = fraction.unwrap_or_default().as_bytes();
        let mut fractional_micro_usd = 0_u64;
        for digit in fraction.iter().take(6) {
            fractional_micro_usd = fractional_micro_usd * 10 + u64::from(digit - b'0');
        }
        for _ in fraction.len()..6 {
            fractional_micro_usd *= 10;
        }
        let rounds_up = fraction
            .get(6..)
            .is_some_and(|remaining| remaining.iter().any(|digit| *digit != b'0'));

        whole
            .checked_mul(1_000_000)
            .and_then(|value| value.checked_add(fractional_micro_usd))
            .and_then(|value| value.checked_add(u64::from(rounds_up)))
            .map(Self)
            .ok_or(ContractError::InvalidValue {
                field: "USD decimal",
                reason: "exceeds u64 micro-USD",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::MicroUsd;

    #[test]
    fn usd_decimal_rounds_sub_micro_usd_remainders_up() {
        assert_eq!(
            MicroUsd::parse_decimal_usd("1.000000"),
            Ok(MicroUsd::new(1_000_000))
        );
        assert_eq!(
            MicroUsd::parse_decimal_usd("0.1"),
            Ok(MicroUsd::new(100_000))
        );
        assert_eq!(
            MicroUsd::parse_decimal_usd("0.000001000000"),
            Ok(MicroUsd::new(1))
        );
        assert_eq!(
            MicroUsd::parse_decimal_usd("0.0000011"),
            Ok(MicroUsd::new(2))
        );
        assert_eq!(
            MicroUsd::parse_decimal_usd("0.0000001"),
            Ok(MicroUsd::new(1))
        );
    }

    #[test]
    fn usd_decimal_rejects_malformed_or_overflowing_amounts() {
        for malformed in ["", ".1", "1.", "-1", "+1", "1e-3", "1.2.3", "0.000000x"] {
            assert!(MicroUsd::parse_decimal_usd(malformed).is_err());
        }
        assert_eq!(
            MicroUsd::parse_decimal_usd("18446744073709.551615"),
            Ok(MicroUsd::new(u64::MAX))
        );
        assert!(MicroUsd::parse_decimal_usd("18446744073709.5516151").is_err());
        assert!(MicroUsd::parse_decimal_usd("18446744073710").is_err());
    }
}

/// A duration carried across a durable boundary as whole milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct DurationMillis(u64);

impl DurationMillis {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Exact BLAKE3 content identity represented as 32 bytes in memory and 64
/// lower-case hexadecimal characters at text boundaries.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ContentDigest")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl FromStr for ContentDigest {
    type Err = ContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err(ContractError::InvalidValue {
                field: "blake3 digest",
                reason: "must contain exactly 64 hexadecimal characters",
            });
        }

        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = hex_nibble(pair[0]).ok_or(ContractError::InvalidValue {
                field: "blake3 digest",
                reason: "must use lower-case hexadecimal characters",
            })?;
            let low = hex_nibble(pair[1]).ok_or(ContractError::InvalidValue {
                field: "blake3 digest",
                reason: "must use lower-case hexadecimal characters",
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
