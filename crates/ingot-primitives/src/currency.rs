use std::{
    fmt,
    ops::{Add, AddAssign, Mul, Sub, SubAssign},
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct CurrencyCode([u8; 8]);

impl CurrencyCode {
    pub fn new(code: &str) -> Self {
        let mut bytes = [0u8; 8];
        for (dst, src) in bytes.iter_mut().zip(code.bytes()) {
            *dst = src.to_ascii_uppercase();
        }
        Self(bytes)
    }

    pub fn as_str(&self) -> &str {
        let len = self.0.iter().position(|&b| b == 0).unwrap_or(8);
        // SAFETY: all constructors (new, from CurrencyCode::new) only write
        // valid ASCII/UTF-8 bytes. The empty-string fallback is a defensive
        // guard against corrupt data arriving via IPC shared memory.
        std::str::from_utf8(&self.0[..len]).unwrap_or("")
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CurrencyCode({:?})", self.as_str())
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Self::new(&s))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(C)]
pub struct Currency {
    pub code: CurrencyCode,
    pub decimals: u8,
}

impl Currency {
    pub fn new(code: &str, decimals: u8) -> Self {
        Self {
            code: CurrencyCode::new(code),
            decimals,
        }
    }

    pub fn usd() -> Self {
        Self::new("USD", 2)
    }

    pub fn btc() -> Self {
        Self::new("BTC", 8)
    }

    /// Construct a `Currency` from a [`CurrencyCode`], inferring the decimal
    /// precision from a built-in lookup of well-known fiat and crypto codes.
    ///
    /// Unknown codes default to 8 decimals (common for most crypto assets).
    pub fn from_code(code: CurrencyCode) -> Self {
        let decimals = match code.as_str() {
            "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "CHF" => 2,
            "JPY" => 0,
            "ETH" => 18,
            "SOL" => 9,
            _ => 8,
        };
        Self { code, decimals }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
#[serde(transparent)]
pub struct Amount(Decimal);

impl Amount {
    pub const ZERO: Self = Self(Decimal::ZERO);

    pub fn is_sign_negative(self) -> bool {
        self.0.is_sign_negative()
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Amount {
    fn from(d: Decimal) -> Self {
        Self(d)
    }
}

impl From<Amount> for Decimal {
    fn from(a: Amount) -> Self {
        a.0
    }
}

impl Add<Amount> for Amount {
    type Output = Amount;

    fn add(self, rhs: Amount) -> Amount {
        Amount(self.0 + rhs.0)
    }
}

impl Sub<Amount> for Amount {
    type Output = Amount;

    fn sub(self, rhs: Amount) -> Amount {
        Amount(self.0 - rhs.0)
    }
}

impl AddAssign<Amount> for Amount {
    fn add_assign(&mut self, rhs: Amount) {
        self.0 += rhs.0;
    }
}

impl SubAssign<Amount> for Amount {
    fn sub_assign(&mut self, rhs: Amount) {
        self.0 -= rhs.0;
    }
}

impl Mul<Price> for Amount {
    type Output = Amount;

    fn mul(self, rhs: Price) -> Amount {
        Amount(self.0 * rhs.0)
    }
}

impl Mul<Decimal> for Amount {
    type Output = Amount;

    fn mul(self, rhs: Decimal) -> Amount {
        Amount(self.0 * rhs)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
#[serde(transparent)]
pub struct Price(Decimal);

impl Price {
    pub fn is_positive(self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn inverse(self) -> Decimal {
        Decimal::ONE / self.0
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Price {
    fn from(d: Decimal) -> Self {
        Self(d)
    }
}

impl From<Price> for Decimal {
    fn from(p: Price) -> Self {
        p.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
#[serde(transparent)]
pub struct Quantity(Decimal);

impl Quantity {
    pub fn is_sign_negative(self) -> bool {
        self.0.is_sign_negative()
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Decimal> for Quantity {
    fn from(d: Decimal) -> Self {
        Self(d)
    }
}

impl From<Quantity> for Decimal {
    fn from(q: Quantity) -> Self {
        q.0
    }
}

impl From<Quantity> for Amount {
    fn from(q: Quantity) -> Self {
        Self(q.0)
    }
}

impl Mul<Price> for Quantity {
    type Output = Amount;

    fn mul(self, rhs: Price) -> Amount {
        Amount(self.0 * rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct Money {
    pub amount: Amount,
    pub currency: Currency,
}

impl Money {
    pub fn new(amount: Amount, currency: Currency) -> Self {
        Self { amount, currency }
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};

    use anyhow::{Context, Result};
    use rust_decimal::dec;

    use super::*;

    #[test]
    fn test_currency_new_uppercases_code() {
        let currency = Currency::new("eur", 2);
        assert_eq!(currency.code.as_str(), "EUR");
        assert_eq!(currency.decimals, 2);
    }

    #[test]
    fn test_currency_defaults() {
        let usd = Currency::usd();
        assert_eq!(usd.code.as_str(), "USD");
        assert_eq!(usd.decimals, 2);

        let btc = Currency::btc();
        assert_eq!(btc.code.as_str(), "BTC");
        assert_eq!(btc.decimals, 8);
    }

    #[test]
    fn test_currency_display_trait() {
        let currency = Currency::new("JPY", 0);
        assert_eq!(format!("{currency}"), "JPY");
    }

    #[test]
    fn test_currency_equality() {
        let c1 = Currency::new("USD", 2);
        let c2 = Currency::usd();
        let c3 = Currency::new("BTC", 8);

        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
    }

    #[test]
    fn test_currency_copy() {
        let c1 = Currency::usd();
        let c2 = c1;
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_currency_hash() {
        let c1 = Currency::usd();
        let c2 = Currency::new("USD", 2);

        let mut hasher1 = DefaultHasher::new();
        c1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        c2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_currency_serialization() -> Result<()> {
        let c = Currency::usd();
        let serialized = serde_json::to_string(&c).context("Failed to serialize")?;

        assert!(serialized.contains("USD"));
        assert!(serialized.contains('2'));

        let deserialized: Currency =
            serde_json::from_str(&serialized).context("Failed to deserialize")?;
        assert_eq!(c, deserialized);
        Ok(())
    }

    #[test]
    fn test_amount_arithmetic() {
        let a = Amount::from(dec!(100));
        let b = Amount::from(dec!(50));

        assert_eq!(a + b, Amount::from(dec!(150)));
        assert_eq!(a - b, Amount::from(dec!(50)));

        let mut c = Amount::from(dec!(10));
        c += Amount::from(dec!(5));
        assert_eq!(c, Amount::from(dec!(15)));
        c -= Amount::from(dec!(3));
        assert_eq!(c, Amount::from(dec!(12)));
    }

    #[test]
    fn test_amount_mul_price() {
        let amount = Amount::from(dec!(2));
        let price = Price::from(dec!(50000));
        assert_eq!(amount * price, Amount::from(dec!(100000)));
    }

    #[test]
    fn test_amount_mul_decimal() {
        let amount = Amount::from(dec!(100000));
        let inverse = dec!(0.00002);
        assert_eq!(amount * inverse, Amount::from(dec!(2)));
    }

    #[test]
    fn test_amount_zero_and_sign() {
        assert_eq!(Amount::ZERO, Amount::from(dec!(0)));
        assert!(Amount::from(dec!(-1)).is_sign_negative());
        assert!(!Amount::from(dec!(1)).is_sign_negative());
    }

    #[test]
    fn test_price_is_positive_and_inverse() {
        let p = Price::from(dec!(50000));
        assert!(p.is_positive());
        assert!(!Price::from(dec!(0)).is_positive());
        assert!(!Price::from(dec!(-1)).is_positive());

        let inv = Price::from(dec!(2)).inverse();
        assert_eq!(inv, dec!(0.5));
    }

    #[test]
    fn test_quantity_mul_price() {
        let qty = Quantity::from(dec!(3));
        let price = Price::from(dec!(1000));
        assert_eq!(qty * price, Amount::from(dec!(3000)));
    }

    #[test]
    fn test_quantity_into_amount() {
        let qty = Quantity::from(dec!(1.5));
        let amt = Amount::from(qty);
        assert_eq!(amt, Amount::from(dec!(1.5)));
    }

    #[test]
    fn test_amount_serialization() -> Result<()> {
        let a = Amount::from(dec!(123.45));
        let serialized = serde_json::to_string(&a).context("Failed to serialize")?;
        let deserialized: Amount =
            serde_json::from_str(&serialized).context("Failed to deserialize")?;
        assert_eq!(a, deserialized);
        Ok(())
    }

    #[test]
    fn test_money_creation() {
        let amount = Amount::from(dec!(100.50));
        let currency = Currency::usd();
        let money = Money::new(amount, currency);

        assert_eq!(money.amount, amount);
        assert_eq!(money.currency, currency);
    }

    #[test]
    fn test_money_equality() {
        let m1 = Money::new(Amount::from(dec!(50.0)), Currency::usd());
        let m2 = Money::new(Amount::from(dec!(50.0)), Currency::usd());
        let m3 = Money::new(Amount::from(dec!(50.0)), Currency::btc());
        let m4 = Money::new(Amount::from(dec!(50.1)), Currency::usd());

        assert_eq!(m1, m2);
        assert_ne!(m1, m3);
        assert_ne!(m1, m4);
    }

    #[test]
    fn test_money_copy() {
        let m1 = Money::new(Amount::from(dec!(100)), Currency::btc());
        let m2 = m1; // Copy
        assert_eq!(m1, m2); // m1 still usable after copy
    }

    #[test]
    fn test_money_serialization() -> Result<()> {
        let m = Money::new(Amount::from(dec!(123.45)), Currency::usd());
        let serialized = serde_json::to_string(&m).context("Failed to serialize")?;

        let deserialized: Money =
            serde_json::from_str(&serialized).context("Failed to deserialize")?;
        assert_eq!(m, deserialized);
        assert_eq!(deserialized.amount, Amount::from(dec!(123.45)));
        Ok(())
    }

    #[test]
    fn test_currency_from_code_fiat() {
        let usd = Currency::from_code(CurrencyCode::new("USD"));
        assert_eq!(usd, Currency::usd());
        assert_eq!(usd.decimals, 2);

        let eur = Currency::from_code(CurrencyCode::new("EUR"));
        assert_eq!(eur.decimals, 2);

        let gbp = Currency::from_code(CurrencyCode::new("GBP"));
        assert_eq!(gbp.decimals, 2);
    }

    #[test]
    fn test_currency_from_code_btc() {
        let btc = Currency::from_code(CurrencyCode::new("BTC"));
        assert_eq!(btc, Currency::btc());
        assert_eq!(btc.decimals, 8);
    }

    #[test]
    fn test_currency_from_code_eth() {
        let eth = Currency::from_code(CurrencyCode::new("ETH"));
        assert_eq!(eth.decimals, 18);
        assert_eq!(eth.code.as_str(), "ETH");
    }

    #[test]
    fn test_currency_from_code_sol() {
        let sol = Currency::from_code(CurrencyCode::new("SOL"));
        assert_eq!(sol.decimals, 9);
    }

    #[test]
    fn test_currency_from_code_unknown_defaults_to_8() {
        let unknown = Currency::from_code(CurrencyCode::new("XYZ"));
        assert_eq!(unknown.decimals, 8);
    }
}
