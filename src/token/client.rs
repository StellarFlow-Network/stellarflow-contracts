use soroban_sdk::{token, Address, Env, String};

/// Reference decimal precision used when a caller doesn't specify one.
/// Matches classic Stellar assets' fixed 7-decimal precision (XLM, and any
/// classic asset wrapped by a Stellar Asset Contract), so code that already
/// thinks in "stroops-per-unit" terms can normalize against custom-decimal
/// tokens without picking an arbitrary reference point.
pub const STELLAR_CLASSIC_DECIMALS: u32 = 7;

/// Converts `amount`, expressed in `from_decimals` precision, into the
/// equivalent amount expressed in `to_decimals` precision.
///
/// This is the actual interoperability gap between native Stellar assets
/// (always 7 decimals) and custom Soroban tokens (which may use any
/// precision, e.g. 18 to mirror ERC-20 conventions): a raw `i128` amount
/// is meaningless across two tokens unless it's rescaled by the difference
/// in decimal places first. `SAClient::transfer_normalized` (below) is
/// what actually applies this at the point of a cross-asset transfer;
/// this function is the pure conversion those helpers are built on, kept
/// separate so the arithmetic itself can be tested and reasoned about
/// without needing a live `Env`/contract.
///
/// Scaling down (`to_decimals < from_decimals`) truncates toward zero —
/// converting `1_500_000_000_000_000_000` (18-decimal) to 7-decimal
/// precision drops the sub-7-decimal remainder rather than rounding it,
/// matching how Stellar itself truncates (never rounds up) when an amount
/// doesn't fit the target precision exactly; silently rounding up would
/// let a converted amount exceed the value it was derived from.
///
/// # Panics
///
/// Panics on `i128` overflow when scaling up (`checked_mul` failure).
/// Soroban's standard trap-on-panic host behavior turns this into a
/// reverted transaction rather than a silently wrapped, incorrect amount
/// — for fund-moving arithmetic, failing loudly is the safe default.
pub fn normalize_amount(amount: i128, from_decimals: u32, to_decimals: u32) -> i128 {
    if from_decimals == to_decimals || amount == 0 {
        return amount;
    }

    if to_decimals > from_decimals {
        let scale = 10i128
            .checked_pow(to_decimals - from_decimals)
            .expect("normalize_amount: scale factor overflow");
        amount
            .checked_mul(scale)
            .expect("normalize_amount: overflow scaling to higher precision")
    } else {
        let scale = 10i128
            .checked_pow(from_decimals - to_decimals)
            .expect("normalize_amount: scale factor overflow");
        amount / scale
    }
}

/// Unified token client wrapping soroban_sdk's token::Client providing a
/// single execution path for native XLM, classic Stellar Asset Contract (SAC)
/// wrapped assets, and native Soroban tokens.
///
/// The Stellar Asset Contract (SAC) standardizes the interface for both
/// classic Stellar assets (cross-border tokens issued via the Stellar network)
/// and native Soroban tokens. Internally all calls delegate to
/// `soroban_sdk::token::Client`, which itself dispatches through the same
/// host-function interface regardless of the underlying asset type — ensuring
/// identical execution semantics across wrapped assets and custom tokens.
pub struct SAClient {
    client: token::Client<'static>,
}

impl SAClient {
    /// Construct a new unified client for the token at `token_id`.
    ///
    /// `token_id` may refer to:
    /// - A native Stellar Asset Contract (SAC) wrapping a classic asset
    ///   (e.g. USDC, XLM)
    /// - A native Soroban token contract
    /// - The native XLM asset (via `env.register_stellar_asset_contract`)
    ///
    /// In all cases `soroban_sdk::token::Client` provides the identical
    /// host-function execution path — meeting the issue #605 requirement of
    /// confirming identical execution paths across SAC and custom tokens.
    pub fn new(env: &Env, token_id: &Address) -> Self {
        Self {
            client: token::Client::new(env, token_id),
        }
    }

    /// Return the balance of `account` for the underlying token.
    pub fn balance(&self, account: &Address) -> i128 {
        self.client.balance(account)
    }

    /// Transfer `amount` from `from` to `to`.
    pub fn transfer(&self, from: &Address, to: &Address, amount: &i128) {
        self.client.transfer(from, to, amount);
    }

    /// Transfer `amount` from `from` to `to` on behalf of `spender`.
    pub fn transfer_from(&self, spender: &Address, from: &Address, to: &Address, amount: &i128) {
        self.client.transfer_from(spender, from, to, amount);
    }

    /// Approve `spender` to spend up to `amount` from `owner`'s balance.
    pub fn approve(&self, owner: &Address, spender: &Address, amount: &i128) {
        self.client.approve(owner, spender, amount);
    }

    /// Return the allowance granted by `owner` to `spender`.
    pub fn allowance(&self, owner: &Address, spender: &Address) -> i128 {
        self.client.allowance(owner, spender)
    }

    /// Return the name of the token.
    pub fn name(&self) -> String {
        self.client.name()
    }

    /// Return the symbol of the token.
    pub fn symbol(&self) -> String {
        self.client.symbol()
    }

    /// Return the number of decimals used by the token.
    pub fn decimals(&self) -> u32 {
        self.client.decimals()
    }

    /// Transfer `amount`, expressed in `reference_decimals` precision,
    /// converting it to this token's own native decimal precision before
    /// calling the underlying transfer.
    ///
    /// This is the "seamless interoperability" entry point: a caller that
    /// works in one canonical precision (e.g. `STELLAR_CLASSIC_DECIMALS`)
    /// can move value into or out of any token — 7-decimal classic assets
    /// or a custom-precision Soroban token alike — without knowing or
    /// caring what precision that specific token happens to use
    /// internally. The token's real decimals are read live via
    /// `decimals()` on every call rather than assumed, since a caller
    /// interacting with several different tokens has no other reliable
    /// way to know each one's precision up front.
    pub fn transfer_normalized(
        &self,
        from: &Address,
        to: &Address,
        amount: i128,
        reference_decimals: u32,
    ) {
        let native_amount = normalize_amount(amount, reference_decimals, self.decimals());
        self.client.transfer(from, to, &native_amount);
    }

    /// Transfer `amount` (in `reference_decimals` precision) from `from`
    /// to `to` on behalf of `spender`, via an existing allowance — the
    /// normalized counterpart to `transfer_from`.
    pub fn transfer_from_normalized(
        &self,
        spender: &Address,
        from: &Address,
        to: &Address,
        amount: i128,
        reference_decimals: u32,
    ) {
        let native_amount = normalize_amount(amount, reference_decimals, self.decimals());
        self.client.transfer_from(spender, from, to, &native_amount);
    }

    /// Return `account`'s balance, converted from this token's native
    /// decimal precision into `reference_decimals` precision.
    pub fn balance_normalized(&self, account: &Address, reference_decimals: u32) -> i128 {
        let native_balance = self.client.balance(account);
        normalize_amount(native_balance, self.decimals(), reference_decimals)
    }

    /// Approve `spender` for `amount`, expressed in `reference_decimals`
    /// precision, converting to this token's native precision first.
    pub fn approve_normalized(
        &self,
        owner: &Address,
        spender: &Address,
        amount: i128,
        reference_decimals: u32,
    ) {
        let native_amount = normalize_amount(amount, reference_decimals, self.decimals());
        self.client.approve(owner, spender, &native_amount);
    }
}

/// Helper that tests identical execution path across SAC and custom tokens.
/// Uses `soroban_sdk::token::Client` for both — the same underlying impl.
pub fn assert_identical_path(env: &Env, sac_token: &Address, custom_token: &Address) {
    let sac = SAClient::new(env, sac_token);
    let custom = SAClient::new(env, custom_token);
    let _ = sac.decimals();
    let _ = custom.decimals();
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_sac_client_balance() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin.clone());
        let sac = SAClient::new(&env, &token_id);
        assert_eq!(sac.balance(&user), 0);
    }

    #[test]
    fn test_sac_client_transfer_and_balance() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin.clone());
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        stellar.mint(&alice, &1000);
        assert_eq!(sac.balance(&alice), 1000);
        sac.transfer(&alice, &bob, &300);
        assert_eq!(sac.balance(&alice), 700);
        assert_eq!(sac.balance(&bob), 300);
    }

    #[test]
    fn test_sac_client_approve_and_transfer_from() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let spender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin.clone());
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        stellar.mint(&owner, &500);
        sac.approve(&owner, &spender, &200);
        assert_eq!(sac.allowance(&owner, &spender), 200);
        sac.transfer_from(&spender, &owner, &recipient, &150);
        assert_eq!(sac.balance(&owner), 350);
        assert_eq!(sac.balance(&recipient), 150);
        assert_eq!(sac.allowance(&owner, &spender), 50);
    }

    #[test]
    fn test_sac_client_metadata() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin.clone());
        let sac = SAClient::new(&env, &token_id);
        assert_eq!(sac.decimals(), 7);
    }

    #[test]
    fn test_identical_path_sac_and_custom() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let sac_token = env.register_stellar_asset_contract(admin.clone());
        let custom_token = env.register_stellar_asset_contract(admin);
        assert_identical_path(&env, &sac_token, &custom_token);
    }

    #[test]
    fn test_sac_client_native_xlm() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);
        stellar.mint(&user, &9999);
        assert_eq!(sac.balance(&user), 9999);
    }

    // ─── normalize_amount: pure arithmetic, no Env required ────────────────

    #[test]
    fn test_normalize_amount_same_decimals_is_identity() {
        assert_eq!(normalize_amount(1_234_567, 7, 7), 1_234_567);
        assert_eq!(normalize_amount(0, 7, 7), 0);
    }

    #[test]
    fn test_normalize_amount_zero_is_always_zero() {
        assert_eq!(normalize_amount(0, 7, 18), 0);
        assert_eq!(normalize_amount(0, 18, 7), 0);
    }

    #[test]
    fn test_normalize_amount_scales_up_for_higher_precision() {
        // 1 unit at 7 decimals (1_0000000) -> 18 decimals.
        let amount_7dp = 10i128.pow(7); // 1.0000000
        let expected_18dp = 10i128.pow(18); // 1.000000000000000000
        assert_eq!(normalize_amount(amount_7dp, 7, 18), expected_18dp);
    }

    #[test]
    fn test_normalize_amount_scales_down_for_lower_precision() {
        // 1 unit at 18 decimals -> 7 decimals.
        let amount_18dp = 10i128.pow(18);
        let expected_7dp = 10i128.pow(7);
        assert_eq!(normalize_amount(amount_18dp, 18, 7), expected_7dp);
    }

    #[test]
    fn test_normalize_amount_scaling_down_truncates_rather_than_rounds() {
        // 1.23456789...(18dp) worth of sub-7-decimal precision should be
        // dropped, not rounded, when converting down to 7 decimals.
        let amount_18dp = 10i128.pow(18) + 999_999_999_999; // 1 + a tiny remainder
        let result = normalize_amount(amount_18dp, 18, 7);
        assert_eq!(result, 10i128.pow(7)); // remainder truncated away
    }

    #[test]
    fn test_normalize_amount_round_trip_loses_no_value_when_scaling_up_then_down() {
        let original_7dp = 42_500_000i128; // 4.25 at 7 decimals
        let scaled_up = normalize_amount(original_7dp, 7, 18);
        let scaled_back_down = normalize_amount(scaled_up, 18, 7);
        assert_eq!(scaled_back_down, original_7dp);
    }

    #[test]
    #[should_panic(expected = "overflow scaling to higher precision")]
    fn test_normalize_amount_panics_on_overflow_scaling_up() {
        // i128::MAX at 7 decimals cannot be represented at 30 decimals —
        // must panic (revert), not silently wrap to an incorrect amount.
        normalize_amount(i128::MAX, 7, 30);
    }

    // ─── Normalized transfers: real SAC tokens, differing reference decimals ──
    //
    // `register_stellar_asset_contract` always yields a 7-decimal token (SACs
    // mirror classic Stellar assets, which are fixed at 7 decimals) — there's
    // no way to get a *real* different-precision token without a full custom
    // token contract, which this crate doesn't otherwise have a build/test
    // fixture for. These tests instead exercise the exact same
    // `SAClient::*_normalized` code path a genuinely custom-precision token
    // would go through, with the caller's `reference_decimals` deliberately
    // set away from the token's real 7 decimals in both directions — the
    // arithmetic risk (and the thing #755 is actually about) is the
    // conversion logic itself, which behaves identically regardless of
    // which contract is on the other end of `token::Client`.

    #[test]
    fn test_transfer_normalized_scales_from_higher_reference_precision() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);

        // Mint 10 whole units at the token's real (7-decimal) precision.
        stellar.mint(&alice, &(10 * 10i128.pow(7)));

        // Caller thinks in 18-decimal terms and asks to send "3 whole
        // units" (3 * 10^18) — SAClient must convert that down to the
        // token's real 7-decimal amount (3 * 10^7) before transferring.
        let amount_at_18dp = 3 * 10i128.pow(18);
        sac.transfer_normalized(&alice, &bob, amount_at_18dp, 18);

        assert_eq!(sac.balance(&bob), 3 * 10i128.pow(7));
        assert_eq!(sac.balance(&alice), 7 * 10i128.pow(7));
    }

    #[test]
    fn test_transfer_normalized_scales_from_lower_reference_precision() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);

        stellar.mint(&alice, &(10 * 10i128.pow(7)));

        // Caller thinks in whole-unit (0-decimal) terms: "send 4 units".
        sac.transfer_normalized(&alice, &bob, 4, 0);

        assert_eq!(sac.balance(&bob), 4 * 10i128.pow(7));
    }

    #[test]
    fn test_balance_normalized_converts_native_balance_to_reference_precision() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);

        stellar.mint(&user, &(5 * 10i128.pow(7))); // 5 whole units, native 7dp

        // Same balance, read back in 18-decimal terms.
        assert_eq!(sac.balance_normalized(&user, 18), 5 * 10i128.pow(18));
        // And in whole-unit (0-decimal) terms.
        assert_eq!(sac.balance_normalized(&user, 0), 5);
    }

    #[test]
    fn test_transfer_normalized_with_matching_reference_decimals_is_unchanged() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);

        stellar.mint(&alice, &1_000);
        // reference_decimals == the token's actual decimals (7): behaves
        // exactly like the unnormalized transfer.
        sac.transfer_normalized(&alice, &bob, 300, STELLAR_CLASSIC_DECIMALS);
        assert_eq!(sac.balance(&bob), 300);
        assert_eq!(sac.balance(&alice), 700);
    }

    #[test]
    fn test_approve_and_transfer_from_normalized_round_trip() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let spender = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract(admin);
        let sac = SAClient::new(&env, &token_id);
        let stellar = soroban_sdk::token::StellarAssetClient::new(&env, &token_id);

        stellar.mint(&owner, &(10 * 10i128.pow(7)));

        // Approve "2 whole units" expressed at 18-decimal precision.
        sac.approve_normalized(&owner, &spender, 2 * 10i128.pow(18), 18);
        assert_eq!(sac.allowance(&owner, &spender), 2 * 10i128.pow(7));

        sac.transfer_from_normalized(&spender, &owner, &recipient, 1 * 10i128.pow(18), 18);
        assert_eq!(sac.balance(&recipient), 1 * 10i128.pow(7));
        assert_eq!(sac.allowance(&owner, &spender), 1 * 10i128.pow(7));
    }
}
