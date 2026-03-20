use std::{
    fmt,
    ops::{Add, Mul, Neg, Sub},
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::PrimitiveError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Price(Decimal);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Quantity(Decimal);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Amount(Decimal);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Percentage(Decimal);

impl Price {
    pub fn new(value: Decimal) -> Self {
        Self(value)
    }

    pub fn value(self) -> Decimal {
        self.0
    }
}

impl Quantity {
    pub fn new(value: Decimal) -> Result<Self, PrimitiveError> {
        if value < Decimal::ZERO {
            return Err(PrimitiveError::InvalidQuantity(value));
        }
        Ok(Self(value))
    }

    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    pub fn value(self) -> Decimal {
        self.0
    }
}

impl Amount {
    pub fn new(value: Decimal) -> Self {
        Self(value)
    }

    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    pub fn value(self) -> Decimal {
        self.0
    }
}

impl Percentage {
    pub fn new(value: Decimal) -> Result<Self, PrimitiveError> {
        if value < Decimal::ZERO || value > Decimal::ONE {
            return Err(PrimitiveError::InvalidPercentage(value));
        }
        Ok(Self(value))
    }

    pub fn value(self) -> Decimal {
        self.0
    }
}

// Price * Quantity -> Amount
impl Mul<Quantity> for Price {
    type Output = Amount;

    fn mul(self, rhs: Quantity) -> Amount {
        Amount(self.0 * rhs.0)
    }
}

// Amount + Amount -> Amount
impl Add for Amount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

// Amount - Amount -> Amount
impl Sub for Amount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

// Amount * Percentage -> Amount
impl Mul<Percentage> for Amount {
    type Output = Amount;

    fn mul(self, rhs: Percentage) -> Amount {
        Amount(self.0 * rhs.0)
    }
}

// Quantity + Quantity -> Quantity
impl Add for Quantity {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

// Quantity - Quantity -> Quantity (can go negative for delta computations, but
// Quantity::new still validates)
impl Sub for Quantity {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

// Negate Amount (for debits/credits)
impl Neg for Amount {
    type Output = Self;

    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Percentage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0 * Decimal::ONE_HUNDRED)
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    // --- Construction tests ---

    #[test]
    fn test_price_new() {
        let p = Price::new(dec!(100.50));
        assert_eq!(p.value(), dec!(100.50));
    }

    #[test]
    fn test_price_negative_allowed() {
        let p = Price::new(dec!(-5.0));
        assert_eq!(p.value(), dec!(-5.0));
    }

    #[test]
    fn test_quantity_new_valid() -> Result<(), PrimitiveError> {
        let q = Quantity::new(dec!(10.5))?;
        assert_eq!(q.value(), dec!(10.5));
        Ok(())
    }

    #[test]
    fn test_quantity_new_zero() -> Result<(), PrimitiveError> {
        let q = Quantity::new(dec!(0))?;
        assert_eq!(q.value(), Decimal::ZERO);
        Ok(())
    }

    #[test]
    fn test_quantity_new_negative_rejected() {
        let result = Quantity::new(dec!(-1));
        assert!(result.is_err());
    }

    #[test]
    fn test_amount_new() {
        let a = Amount::new(dec!(1000.00));
        assert_eq!(a.value(), dec!(1000.00));
    }

    #[test]
    fn test_percentage_new_valid() -> Result<(), PrimitiveError> {
        let p = Percentage::new(dec!(0.5))?;
        assert_eq!(p.value(), dec!(0.5));
        Ok(())
    }

    #[test]
    fn test_percentage_new_zero() -> Result<(), PrimitiveError> {
        let p = Percentage::new(dec!(0))?;
        assert_eq!(p.value(), Decimal::ZERO);
        Ok(())
    }

    #[test]
    fn test_percentage_new_one() -> Result<(), PrimitiveError> {
        let p = Percentage::new(dec!(1))?;
        assert_eq!(p.value(), Decimal::ONE);
        Ok(())
    }

    #[test]
    fn test_percentage_new_above_one_rejected() {
        let result = Percentage::new(dec!(1.01));
        assert!(result.is_err());
    }

    #[test]
    fn test_percentage_new_negative_rejected() {
        let result = Percentage::new(dec!(-0.1));
        assert!(result.is_err());
    }

    // --- Arithmetic tests ---

    #[test]
    fn test_price_times_quantity_gives_amount() -> Result<(), PrimitiveError> {
        let price = Price::new(dec!(50.25));
        let qty = Quantity::new(dec!(10))?;
        let amount = price * qty;
        assert_eq!(amount.value(), dec!(502.50));
        Ok(())
    }

    #[test]
    fn test_amount_add() {
        let a = Amount::new(dec!(100));
        let b = Amount::new(dec!(50.50));
        assert_eq!((a + b).value(), dec!(150.50));
    }

    #[test]
    fn test_amount_sub() {
        let a = Amount::new(dec!(100));
        let b = Amount::new(dec!(30));
        assert_eq!((a - b).value(), dec!(70));
    }

    #[test]
    fn test_amount_neg() {
        let a = Amount::new(dec!(100));
        assert_eq!((-a).value(), dec!(-100));
    }

    #[test]
    fn test_amount_times_percentage() -> Result<(), PrimitiveError> {
        let amount = Amount::new(dec!(1000));
        let pct = Percentage::new(dec!(0.1))?;
        assert_eq!((amount * pct).value(), dec!(100));
        Ok(())
    }

    #[test]
    fn test_quantity_add() -> Result<(), PrimitiveError> {
        let a = Quantity::new(dec!(5))?;
        let b = Quantity::new(dec!(3))?;
        assert_eq!((a + b).value(), dec!(8));
        Ok(())
    }

    // --- Display tests ---

    #[test]
    fn test_price_display() {
        let p = Price::new(dec!(100.50));
        assert_eq!(p.to_string(), "100.50");
    }

    #[test]
    fn test_percentage_display() -> Result<(), PrimitiveError> {
        let p = Percentage::new(dec!(0.15))?;
        assert_eq!(p.to_string(), "15.00%");
        Ok(())
    }

    // --- Serde round-trip tests ---

    #[test]
    fn test_price_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let price = Price::new(dec!(99.99));
        let json = serde_json::to_string(&price)?;
        let deserialized: Price = serde_json::from_str(&json)?;
        assert_eq!(price, deserialized);
        Ok(())
    }

    #[test]
    fn test_quantity_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let qty = Quantity::new(dec!(42.5))?;
        let json = serde_json::to_string(&qty)?;
        let deserialized: Quantity = serde_json::from_str(&json)?;
        assert_eq!(qty, deserialized);
        Ok(())
    }

    #[test]
    fn test_amount_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let amount = Amount::new(dec!(1234.5678));
        let json = serde_json::to_string(&amount)?;
        let deserialized: Amount = serde_json::from_str(&json)?;
        assert_eq!(amount, deserialized);
        Ok(())
    }
}
