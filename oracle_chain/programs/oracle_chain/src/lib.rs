use anchor_lang::prelude::*;
use pyth_sdk_solana::{load_price_feed_from_account_info, PriceFeed};

declare_id!("OracLe1111111111111111111111111111111111");

#[program]
pub mod oracle_integration {
    use super::*;


    pub fn get_pyth_price(ctx: Context<GetPythPrice>) -> Result<PriceData> {
        let price_feed: PriceFeed =
            load_price_feed_from_account_info(&ctx.accounts.pyth_feed)?;

        let price_data = price_feed
            .get_current_price()
            .ok_or(OracleError::PriceUnavailable)?;


        let clock = Clock::get()?;
        require!(
            clock.unix_timestamp - price_data.publish_time <= ctx.accounts.config.max_staleness,
            OracleError::PriceStale
        );

        require!(
            price_data.conf <= ctx.accounts.config.max_confidence as i64,
            OracleError::ConfidenceTooHigh
        );

        Ok(PriceData {
            price: price_data.price,
            confidence: price_data.conf as u64,
            expo: price_data.expo,
            timestamp: price_data.publish_time,
            source: PriceSource::Pyth,
        })
    }

    // =============================
    // ✅ READ SWITCHBOARD PRICE
    // =============================
    pub fn get_switchboard_price(ctx: Context<GetSwitchboardPrice>) -> Result<PriceData> {
        let aggregator = &ctx.accounts.switchboard_aggregator;
        let result = aggregator.latest_result;

        let clock = Clock::get()?;
        require!(
            clock.unix_timestamp - result.updated_at <= ctx.accounts.config.max_staleness,
            OracleError::PriceStale
        );

        Ok(PriceData {
            price: result.value,
            confidence: result.std_dev as u64,
            expo: result.scale,
            timestamp: result.updated_at,
            source: PriceSource::Switchboard,
        })
    }

    pub fn validate_price_consensus(
        _ctx: Context<ValidatePrice>,
        mut prices: Vec<PriceData>,
    ) -> Result<i64> {
        require!(prices.len() >= 2, OracleError::NotEnoughSources);

        prices.sort_by(|a, b| a.price.cmp(&b.price));
        let median = prices[prices.len() / 2].price;

        for p in prices.iter() {
            let deviation = ((p.price - median).abs() * 10_000) / median;
            require!(deviation <= 100, OracleError::PriceDeviation); // 1%
        }

        Ok(median)
    }
}


#[derive(Accounts)]
pub struct GetPythPrice<'info> {
    /// CHECK: Pyth price account
    pub pyth_feed: AccountInfo<'info>,

    #[account(mut)]
    pub config: Account<'info, OracleConfig>,
}

#[derive(Accounts)]
pub struct GetSwitchboardPrice<'info> {
    pub switchboard_aggregator: Account<'info, SwitchboardAggregator>,

    #[account(mut)]
    pub config: Account<'info, OracleConfig>,
}

#[derive(Accounts)]
pub struct ValidatePrice {}




#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PriceData {
    pub price: i64,
    pub confidence: u64,
    pub expo: i32,
    pub timestamp: i64,
    pub source: PriceSource,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum PriceSource {
    Pyth,
    Switchboard,
    Internal,
}

#[account]
pub struct OracleConfig {
    pub symbol: String,
    pub pyth_feed: Pubkey,
    pub switchboard_aggregator: Pubkey,
    pub max_staleness: i64,   // seconds
    pub max_confidence: u64,  // basis points
    pub max_deviation: u64,   // basis points
}


#[account]
pub struct SwitchboardAggregator {
    pub latest_result: SwitchboardResult,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SwitchboardResult {
    pub value: i64,
    pub std_dev: i64,
    pub scale: i32,
    pub updated_at: i64,
}


#[error_code]
pub enum OracleError {
    #[msg("Price unavailable")]
    PriceUnavailable,

    #[msg("Price is stale")]
    PriceStale,

    #[msg("Confidence too high")]
    ConfidenceTooHigh,

    #[msg("Not enough oracle sources")]
    NotEnoughSources,

    #[msg("Price deviation too large")]
    PriceDeviation,
}
