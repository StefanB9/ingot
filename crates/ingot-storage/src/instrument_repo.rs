use anyhow::{Context, Result};
use ingot_core::{Instrument, InstrumentDetails};
use ingot_primitives::{AssetClass, Currency, Exchange, Price, Symbol};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::instrument;

pub struct PgInstrumentRepository {
    pool: PgPool,
}

impl PgInstrumentRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, instrument), fields(symbol = %instrument.symbol))]
    pub async fn upsert(&self, instrument: &Instrument) -> Result<()> {
        let details_json = serde_json::to_value(&instrument.details)
            .context("failed to serialize instrument details")?;

        let symbol = instrument.symbol.as_str();
        let asset_class = instrument.asset_class.to_string();
        let exchange = instrument.exchange.to_string();
        let base_currency = instrument.base_currency.as_str();
        let quote_currency = instrument.quote_currency.as_str();
        let tick_size = instrument.tick_size.value();
        let display_name = instrument.display_name.as_str();

        sqlx::query!(
            "INSERT INTO instruments (symbol, asset_class, exchange, base_currency, \
             quote_currency, tick_size, display_name, details_json, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
            ON CONFLICT (symbol) DO UPDATE SET
                asset_class = EXCLUDED.asset_class,
                exchange = EXCLUDED.exchange,
                base_currency = EXCLUDED.base_currency,
                quote_currency = EXCLUDED.quote_currency,
                tick_size = EXCLUDED.tick_size,
                display_name = EXCLUDED.display_name,
                details_json = EXCLUDED.details_json,
                updated_at = NOW()",
            symbol,
            asset_class,
            exchange,
            base_currency,
            quote_currency,
            tick_size,
            display_name,
            details_json,
        )
        .execute(&self.pool)
        .await
        .context("failed to upsert instrument")?;

        Ok(())
    }

    #[instrument(skip(self), fields(symbol = %symbol))]
    pub async fn get_by_symbol(&self, symbol: &Symbol) -> Result<Option<Instrument>> {
        let sym = symbol.as_str();
        let row = sqlx::query!(
            "SELECT symbol, asset_class, exchange, base_currency, quote_currency, tick_size, \
             display_name, details_json FROM instruments WHERE symbol = $1",
            sym,
        )
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch instrument")?;

        row.map(|r| {
            InstrumentRow {
                symbol: r.symbol,
                asset_class: r.asset_class,
                exchange: r.exchange,
                base_currency: r.base_currency,
                quote_currency: r.quote_currency,
                tick_size: r.tick_size,
                display_name: r.display_name,
                details_json: r.details_json,
            }
            .into_instrument()
        })
        .transpose()
    }

    #[instrument(skip(self), fields(exchange = %exchange))]
    pub async fn list_by_exchange(&self, exchange: Exchange) -> Result<Vec<Instrument>> {
        let exchange_str = exchange.to_string();
        let rows = sqlx::query!(
            "SELECT symbol, asset_class, exchange, base_currency, quote_currency, tick_size, \
             display_name, details_json FROM instruments WHERE exchange = $1",
            exchange_str,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list instruments by exchange")?;

        rows.into_iter()
            .map(|r| {
                InstrumentRow {
                    symbol: r.symbol,
                    asset_class: r.asset_class,
                    exchange: r.exchange,
                    base_currency: r.base_currency,
                    quote_currency: r.quote_currency,
                    tick_size: r.tick_size,
                    display_name: r.display_name,
                    details_json: r.details_json,
                }
                .into_instrument()
            })
            .collect()
    }

    #[instrument(skip(self), fields(asset_class = %asset_class))]
    pub async fn list_by_asset_class(&self, asset_class: AssetClass) -> Result<Vec<Instrument>> {
        let asset_class_str = asset_class.to_string();
        let rows = sqlx::query!(
            "SELECT symbol, asset_class, exchange, base_currency, quote_currency, tick_size, \
             display_name, details_json FROM instruments WHERE asset_class = $1",
            asset_class_str,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list instruments by asset class")?;

        rows.into_iter()
            .map(|r| {
                InstrumentRow {
                    symbol: r.symbol,
                    asset_class: r.asset_class,
                    exchange: r.exchange,
                    base_currency: r.base_currency,
                    quote_currency: r.quote_currency,
                    tick_size: r.tick_size,
                    display_name: r.display_name,
                    details_json: r.details_json,
                }
                .into_instrument()
            })
            .collect()
    }
}

struct InstrumentRow {
    symbol: String,
    asset_class: String,
    exchange: String,
    base_currency: String,
    quote_currency: String,
    tick_size: Decimal,
    display_name: String,
    details_json: serde_json::Value,
}

impl InstrumentRow {
    fn into_instrument(self) -> Result<Instrument> {
        let symbol = Symbol::new(&self.symbol).context("invalid symbol from database")?;
        let asset_class = parse_asset_class(&self.asset_class)?;
        let exchange = parse_exchange(&self.exchange)?;
        let details: InstrumentDetails = serde_json::from_value(self.details_json)
            .context("failed to deserialize instrument details")?;

        Ok(Instrument {
            symbol,
            asset_class,
            exchange,
            base_currency: Currency::from_str_lossy(&self.base_currency),
            quote_currency: Currency::from_str_lossy(&self.quote_currency),
            tick_size: Price::new(self.tick_size),
            display_name: smol_str::SmolStr::new(&self.display_name),
            details,
        })
    }
}

fn parse_asset_class(s: &str) -> Result<AssetClass> {
    match s {
        "Equity" => Ok(AssetClass::Equity),
        "Option" => Ok(AssetClass::Option),
        "Future" => Ok(AssetClass::Future),
        "Forex" => Ok(AssetClass::Forex),
        "CryptoSpot" => Ok(AssetClass::CryptoSpot),
        "CryptoFuture" => Ok(AssetClass::CryptoFuture),
        "Bond" => Ok(AssetClass::Bond),
        other => anyhow::bail!("unknown asset class: {other}"),
    }
}

fn parse_exchange(s: &str) -> Result<Exchange> {
    match s {
        "Kraken" => Ok(Exchange::Kraken),
        "KrakenFutures" => Ok(Exchange::KrakenFutures),
        "IBKR" => Ok(Exchange::IBKR),
        "Paper" => Ok(Exchange::Paper),
        other => anyhow::bail!("unknown exchange: {other}"),
    }
}
