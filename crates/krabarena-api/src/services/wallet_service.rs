//! A user's single crypto wallet: read + validated upsert.
//!
//! One wallet per user (`wallets.user_id` is UNIQUE). Currency + network
//! are server-set from `crypto_networks::default_network()` — the UI
//! offers no network choice today (Toncoin on TON only) but the registry
//! makes adding more a one-line change.

use uuid::Uuid;

use krabarena_shared::field_limits::Field;

use crate::error::ApiError;
use crate::models::wallet::WalletRow;
use crate::services::crypto_networks;

const WALLET_COLUMNS: &str = "id, user_id, address, currency, network, created_at, updated_at";

/// Fetch a user's wallet, if they have one.
pub async fn get_wallet(pool: &sqlx::PgPool, user_id: Uuid) -> sqlx::Result<Option<WalletRow>> {
    let sql = format!("SELECT {WALLET_COLUMNS} FROM wallets WHERE user_id = $1");
    sqlx::query_as::<_, WalletRow>(&sql)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

/// Create or replace a user's wallet address. Validates length (shared
/// `field_limits`) and per-network address format (`crypto_networks`).
/// Currency/network come from the default network — never the client.
pub async fn upsert_wallet(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    address: &str,
) -> Result<WalletRow, ApiError> {
    let address = address.trim();
    // Length bound (also enforced by the DB CHECK in lockstep).
    Field::WalletsAddress.check(address)?;

    let net = crypto_networks::default_network();
    // Per-network format (charset, length, checksum).
    crypto_networks::validate(net.code, address).map_err(ApiError::ValidationError)?;

    let sql = format!(
        "INSERT INTO wallets (user_id, address, currency, network) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (user_id) DO UPDATE \
            SET address = excluded.address, \
                currency = excluded.currency, \
                network = excluded.network, \
                updated_at = now() \
         RETURNING {WALLET_COLUMNS}"
    );
    sqlx::query_as::<_, WalletRow>(&sql)
        .bind(user_id)
        .bind(address)
        .bind(net.currency)
        .bind(net.code)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))
}
