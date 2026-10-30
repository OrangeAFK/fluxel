//! Kernel time as integer femtoseconds.
//!
//! [`Femtos::from_ps_f64_rounded`] is the only `f64` conversion. Call it only
//! where a LUT sample crosses into kernel time.

use std::fmt;
use std::ops::{Add, Sub};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A duration or timestamp in femtoseconds.
///
/// The `i64` range is about ±2.5 hours. Ordering is the integer order, so the
/// same values compare the same on every platform.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Femtos(pub i64);

/// Failure of a checked time conversion or addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TimeError {
    #[error("femtosecond value overflowed")]
    Overflow,
    #[error("picosecond value is not finite")]
    NonFinite,
    #[error("picosecond value does not fit in femtoseconds")]
    OutOfRange,
}

impl Femtos {
    /// `ps` picoseconds, checked against `i64` overflow.
    pub fn from_ps(ps: i64) -> Result<Self, TimeError> {
        ps.checked_mul(1_000).map(Femtos).ok_or(TimeError::Overflow)
    }

    /// `ns` nanoseconds, checked against `i64` overflow.
    pub fn from_ns(ns: i64) -> Result<Self, TimeError> {
        ns.checked_mul(1_000_000)
            .map(Femtos)
            .ok_or(TimeError::Overflow)
    }

    /// Addition that returns [`TimeError::Overflow`] instead of panicking.
    pub fn checked_add(self, rhs: Self) -> Result<Self, TimeError> {
        self.0
            .checked_add(rhs.0)
            .map(Femtos)
            .ok_or(TimeError::Overflow)
    }

    /// Round a picosecond `f64` to the nearest femtosecond, half away from zero.
    ///
    /// This is the only `f64` conversion on [`Femtos`]. Use it only at a LUT
    /// boundary, when a table sample enters the kernel. Non-finite inputs
    /// return [`TimeError::NonFinite`]. Finite values that do not fit in an
    /// `i64` femtosecond count return [`TimeError::OutOfRange`].
    pub fn from_ps_f64_rounded(ps: f64) -> Result<Self, TimeError> {
        if !ps.is_finite() {
            return Err(TimeError::NonFinite);
        }
        let fs = ps * 1000.0;
        if !fs.is_finite() {
            return Err(TimeError::OutOfRange);
        }
        let rounded = fs.round();
        // `i64::MIN` is exactly −2^63. `i64::MAX` is not an `f64`; 2^63 does not fit.
        if rounded < (i64::MIN as f64) || rounded >= (1u64 << 63) as f64 {
            return Err(TimeError::OutOfRange);
        }
        Ok(Femtos(rounded as i64))
    }
}

impl Add for Femtos {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        match self.checked_add(rhs) {
            Ok(sum) => sum,
            Err(_) => panic!("Femtos addition overflowed"),
        }
    }
}

impl Sub for Femtos {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        match self.0.checked_sub(rhs.0) {
            Some(diff) => Femtos(diff),
            None => panic!("Femtos subtraction overflowed"),
        }
    }
}

impl fmt::Display for Femtos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let mag = self.0.unsigned_abs();
        let ps = mag / 1000;
        let frac = mag % 1000;
        write!(f, "{sign}{ps}.{frac:03} ps")
    }
}

#[cfg(test)]
mod tests {
    use super::{Femtos, TimeError};

    #[test]
    fn ps_is_one_thousand_fs() {
        assert_eq!(Femtos::from_ps(2).unwrap(), Femtos(2_000));
    }

    #[test]
    fn ns_is_one_million_fs() {
        assert_eq!(Femtos::from_ns(3).unwrap(), Femtos(3_000_000));
    }

    #[test]
    fn from_ps_overflow_errors() {
        assert_eq!(Femtos::from_ps(i64::MAX), Err(TimeError::Overflow));
    }

    #[test]
    fn from_ns_overflow_errors() {
        assert_eq!(Femtos::from_ns(i64::MAX), Err(TimeError::Overflow));
    }

    #[test]
    fn display_ps_three_decimals() {
        assert_eq!(Femtos(0).to_string(), "0.000 ps");
        assert_eq!(Femtos(1).to_string(), "0.001 ps");
        assert_eq!(Femtos(1500).to_string(), "1.500 ps");
        assert_eq!(Femtos(-2500).to_string(), "-2.500 ps");
        assert_eq!(Femtos(i64::MIN).to_string(), "-9223372036854775.808 ps");
    }

    #[test]
    fn add_sub() {
        let sum = Femtos::from_ps(1).unwrap() + Femtos::from_ps(2).unwrap();
        let diff = Femtos::from_ps(5).unwrap() - Femtos::from_ps(1).unwrap();
        assert_eq!(sum, Femtos::from_ps(3).unwrap());
        assert_eq!(diff, Femtos::from_ps(4).unwrap());
    }

    #[test]
    fn checked_add_ok() {
        assert_eq!(Femtos(1_000).checked_add(Femtos(500)), Ok(Femtos(1_500)));
    }

    #[test]
    fn checked_add_overflow_is_error() {
        assert_eq!(
            Femtos(i64::MAX).checked_add(Femtos(1)),
            Err(TimeError::Overflow)
        );
    }

    #[test]
    #[should_panic]
    fn add_panics_on_overflow() {
        let _ = Femtos(i64::MAX) + Femtos(1);
    }

    #[test]
    fn from_ps_f64_rounded_nearest_fs() {
        assert_eq!(Femtos::from_ps_f64_rounded(1.5).unwrap(), Femtos(1500));
        assert_eq!(Femtos::from_ps_f64_rounded(0.0004).unwrap(), Femtos(0));
        assert_eq!(Femtos::from_ps_f64_rounded(0.0005).unwrap(), Femtos(1));
        assert_eq!(Femtos::from_ps_f64_rounded(-1.5).unwrap(), Femtos(-1500));
        assert_eq!(Femtos::from_ps_f64_rounded(-0.0004).unwrap(), Femtos(0));
        assert_eq!(Femtos::from_ps_f64_rounded(-0.0005).unwrap(), Femtos(-1));
    }

    #[test]
    fn from_ps_f64_rounded_rejects_non_finite() {
        assert_eq!(
            Femtos::from_ps_f64_rounded(f64::NAN),
            Err(TimeError::NonFinite)
        );
        assert_eq!(
            Femtos::from_ps_f64_rounded(f64::INFINITY),
            Err(TimeError::NonFinite)
        );
        assert_eq!(
            Femtos::from_ps_f64_rounded(f64::NEG_INFINITY),
            Err(TimeError::NonFinite)
        );
    }

    #[test]
    fn from_ps_f64_rounded_rejects_out_of_range() {
        assert_eq!(
            Femtos::from_ps_f64_rounded(1e16),
            Err(TimeError::OutOfRange)
        );
    }

    #[test]
    fn serde_femtos_is_i64() {
        let json = serde_json::to_string(&Femtos(1500)).unwrap();
        assert_eq!(json, "1500");
        let back: Femtos = serde_json::from_str("1500").unwrap();
        assert_eq!(back, Femtos(1500));
    }

    proptest::proptest! {
        #[test]
        fn prop_ord_matches_i64(a: i64, b: i64) {
            assert_eq!(Femtos(a).cmp(&Femtos(b)), a.cmp(&b));
        }

        #[test]
        fn prop_checked_add_matches_i64(a: i64, b: i64) {
            let got = Femtos(a).checked_add(Femtos(b));
            match a.checked_add(b) {
                Some(sum) => assert_eq!(got, Ok(Femtos(sum))),
                None => assert_eq!(got, Err(TimeError::Overflow)),
            }
        }
    }
}
