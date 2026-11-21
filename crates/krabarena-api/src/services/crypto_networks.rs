//! Supported crypto currencies / networks for wallet payouts.
//!
//! Single source of truth for which `(currency, network)` pairs a user
//! may attach to their wallet, and how to validate an address for each.
//! Today there is exactly one entry — **Toncoin on the TON network** —
//! but the shape is a registry on purpose: adding TRC-20 (or anything
//! else) later is one new [`NetworkSpec`] in [`SUPPORTED`] plus its
//! validator function. No table or schema changes; `wallets.network`
//! and `payout_transactions.network` already carry the code as data.
//!
//! The per-network `validate` only checks address *format* (charset,
//! length, checksum). Length bounds that the DB also enforces live in
//! `krabarena_shared::field_limits` (`Field::WalletsAddress`).

use base64::Engine;

/// One supported currency/network pair plus its address validator.
pub struct NetworkSpec {
    /// Stable code persisted in `wallets.network` (e.g. `"ton"`).
    pub code: &'static str,
    /// Currency ticker persisted in `wallets.currency` (e.g. `"TON"`).
    pub currency: &'static str,
    /// Human label for the UI (e.g. `"Toncoin · TON"`).
    pub display: &'static str,
    /// Address-format validator. `Ok(())` if the string is a plausible
    /// address on this network; `Err(reason)` otherwise.
    pub validate: fn(&str) -> Result<(), String>,
}

/// Every supported `(currency, network)`. The first entry is the
/// default offered in the UI. Extend here to add networks.
pub const SUPPORTED: &[NetworkSpec] = &[NetworkSpec {
    code: "ton",
    currency: "TON",
    display: "Toncoin · TON",
    validate: validate_ton_address,
}];

/// The default network code (`"ton"`) — used when the client doesn't
/// (and currently can't) choose one.
pub fn default_network() -> &'static NetworkSpec {
    &SUPPORTED[0]
}

/// Look up a network spec by its code.
pub fn by_code(code: &str) -> Option<&'static NetworkSpec> {
    SUPPORTED.iter().find(|n| n.code == code)
}

/// Validate `address` for the network identified by `code`. Returns an
/// error if the network is unknown or the address is malformed.
pub fn validate(code: &str, address: &str) -> Result<(), String> {
    match by_code(code) {
        Some(spec) => (spec.validate)(address),
        None => Err(format!("unsupported network '{code}'")),
    }
}

/// Validate a TON (The Open Network) user-friendly address.
///
/// User-friendly form: 48 base64 / base64url characters that decode to
/// 36 bytes — `[tag(1)][workchain(1)][account(32)][crc16(2)]`. We check
/// length, decode (accepting both base64 and url-safe alphabets), verify
/// the 36-byte length, the tag byte is one of the known flags, and the
/// trailing CRC16-CCITT (XMODEM) matches the first 34 bytes. The
/// checksum is the real integrity guard against typos.
pub fn validate_ton_address(addr: &str) -> Result<(), String> {
    let addr = addr.trim();
    if addr.len() != 48 {
        return Err("TON address must be 48 characters".to_string());
    }

    // Accept both url-safe (-, _) and standard (+, /) base64. Try
    // url-safe first since TON wallets emit that form most often.
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(addr)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(addr))
        .map_err(|_| "TON address is not valid base64".to_string())?;

    if bytes.len() != 36 {
        return Err("TON address has an invalid length after decoding".to_string());
    }

    // Tag byte: 0x11 bounceable / 0x51 non-bounceable, optionally with
    // the 0x80 test-only bit (0x91 / 0xD1).
    let tag = bytes[0];
    if !matches!(tag, 0x11 | 0x51 | 0x91 | 0xD1) {
        return Err("TON address has an unexpected tag byte".to_string());
    }

    let expected = u16::from_be_bytes([bytes[34], bytes[35]]);
    let actual = crc16_ccitt(&bytes[..34]);
    if expected != actual {
        return Err("TON address checksum does not match".to_string());
    }

    Ok(())
}

/// CRC16-CCITT (XMODEM): polynomial 0x1021, initial value 0x0000.
/// This is the variant TON uses for user-friendly address checksums.
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a well-formed TON user-friendly address from a tag + a
    /// 32-byte account, sealing it with the same CRC the validator
    /// checks. Self-consistent, so we never hardcode an external address.
    fn make_ton_address(tag: u8, account: [u8; 32]) -> String {
        let mut bytes = Vec::with_capacity(36);
        bytes.push(tag);
        bytes.push(0x00); // basechain workchain
        // Layout: tag(1) + workchain(1) + account(32) + crc(2) = 36 bytes.
        bytes.extend_from_slice(&account);
        let crc = crc16_ccitt(&bytes);
        bytes.extend_from_slice(&crc.to_be_bytes());
        base64::engine::general_purpose::URL_SAFE.encode(&bytes)
    }

    #[test]
    fn accepts_valid_ton_address() {
        let addr = make_ton_address(0x11, [7u8; 32]);
        assert_eq!(addr.len(), 48);
        assert!(
            validate_ton_address(&addr).is_ok(),
            "should accept a CRC-valid TON address"
        );
    }

    #[test]
    fn accepts_non_bounceable_tag() {
        let addr = make_ton_address(0x51, [3u8; 32]);
        assert!(validate_ton_address(&addr).is_ok());
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(validate_ton_address("EQ123").is_err());
    }

    #[test]
    fn rejects_corrupted_checksum() {
        let addr = make_ton_address(0x11, [7u8; 32]);
        // Flip the last character — breaks the CRC.
        let mut chars: Vec<char> = addr.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let corrupted: String = chars.into_iter().collect();
        assert!(validate_ton_address(&corrupted).is_err());
    }

    #[test]
    fn rejects_bad_tag_byte() {
        // 0x00 is not a recognised tag; CRC is still valid so this
        // exercises the tag check specifically.
        let addr = make_ton_address(0x00, [1u8; 32]);
        assert!(validate_ton_address(&addr).is_err());
    }

    #[test]
    fn rejects_non_base64() {
        // 48 chars but contains characters outside both alphabets.
        let bad = "!".repeat(48);
        assert!(validate_ton_address(&bad).is_err());
    }

    #[test]
    fn validate_dispatches_by_network() {
        let addr = make_ton_address(0x11, [9u8; 32]);
        assert!(validate("ton", &addr).is_ok());
        assert!(validate("dogecoin", &addr).is_err());
    }

    #[test]
    fn crc16_known_vector() {
        // CRC16/XMODEM of "123456789" is 0x31C3.
        assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
    }

    #[test]
    fn default_network_is_ton() {
        assert_eq!(default_network().code, "ton");
        assert_eq!(default_network().currency, "TON");
    }
}
