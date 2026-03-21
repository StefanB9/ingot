use ingot_primitives::{Amount, Currency};
use serde::{Deserialize, Serialize};

use crate::types::AccountId;

/// Aggregated balance for an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyBalance {
    pub account_id: AccountId,
    pub currency: Currency,
    pub balance: Amount,
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Exchange;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::AccountType;

    #[test]
    fn test_currency_balance_construction() -> Result<(), Box<dyn std::error::Error>> {
        let account_id =
            AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                .map_err(|e| format!("{e}"))?;
        let balance = CurrencyBalance {
            account_id: account_id.clone(),
            currency: Currency::USD,
            balance: Amount::new(dec!(1000)),
        };
        assert_eq!(balance.account_id, account_id);
        assert_eq!(balance.currency, Currency::USD);
        assert_eq!(balance.balance, Amount::new(dec!(1000)));
        Ok(())
    }

    #[test]
    fn test_currency_balance_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let account_id =
            AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                .map_err(|e| format!("{e}"))?;
        let balance = CurrencyBalance {
            account_id,
            currency: Currency::USD,
            balance: Amount::new(dec!(1000)),
        };
        let json = serde_json::to_string(&balance)?;
        let deserialized: CurrencyBalance = serde_json::from_str(&json)?;
        assert_eq!(balance, deserialized);
        Ok(())
    }
}
