use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::collections::UnorderedMap;
use near_sdk::json_types::U128;
use near_sdk::{
    assert_one_yocto, env, log, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise,
    PromiseOrValue,
};

// Constants
const WEEK: u64 = 7 * 24 * 60 * 60; // Number of seconds in a week
const STAKE_AMOUNT: u128 = 100_000_000_000_000_000_000; // Default 100 PUBLIC
const NANOSECONDS: u64 = 1_000_000_000; // Nanoseconds to seconds

#[near(serializers = [json, borsh])]
pub struct UserStakeInfo {
    staked: bool,    // Whether the stake conditions are met
    amount: u128,    // The principal amount staked by the user
    start_time: u64, // Timestamp when staking began
}

/// Struct for storing staking information
#[near(serializers = [json, borsh])]
pub struct StakeInfo {
    amount: u128,    // The principal amount staked by the user
    start_time: u64, // Timestamp when staking began
}

#[near(serializers = [json, borsh])]
pub enum UserOperationState {
    Idle,
    Staking,
    Unstaking,
}
/// Main contract struct
#[derive(PanicOnDefault)]
#[near(contract_state)]
pub struct StakingContract {
    owner_id: AccountId,                                      // Contract owner
    token_contract: AccountId,                                // NEP-141 token contract address
    staked_balances: UnorderedMap<AccountId, StakeInfo>,      // User staking information
    user_states: UnorderedMap<AccountId, UserOperationState>, // User operation state
    stake_amount: u128,                                       // Amount required to stake
    lock_duration: u64,                                       // Lock duration
    stake_paused: bool,                                       // Pause stake
    total_staked: u128,                                       // Total amount staked
    total_user: u64,                                          // Total number of staking users
}

#[near]
impl StakingContract {
    /// Initialize the contract
    #[init]
    pub fn new(owner_id: AccountId, token_contract: AccountId) -> Self {
        assert!(!env::state_exists(), "Already initialized");
        Self {
            owner_id,
            token_contract,
            staked_balances: UnorderedMap::new(b"s".to_vec()),
            user_states: UnorderedMap::new(b"user_states".to_vec()),
            stake_paused: false,
            lock_duration: 2 * WEEK, // Lock 2 weeks on default
            stake_amount: STAKE_AMOUNT,
            total_staked: 0,
            total_user: 0,
        }
    }

    /// Pause or start stake (only callable by the owner).
    /// - `pause`: If true, staking is paused, if false, staking is started.
    #[payable]
    pub fn pause_stake(&mut self, pause: bool) {
        assert_one_yocto();
        assert_eq!(
            self.owner_id,
            env::predecessor_account_id(),
            "Only the owner can pause or start stake."
        );
        self.stake_paused = pause;
        env::log_str(&format!("Stake paused updated to {}", self.stake_paused));
    }

    /// Set lock duration (only callable by the owner).
    /// - `lock_duration`: Lock duration.
    #[payable]
    pub fn set_lock_duration(&mut self, lock_duration: u64) {
        assert_one_yocto();
        assert_eq!(
            self.owner_id,
            env::predecessor_account_id(),
            "Only the owner can set lock duration."
        );
        self.lock_duration = lock_duration;
        env::log_str(&format!("Lock duration updated to {}", self.lock_duration));
    }

    #[payable]
    pub fn update_owner(&mut self, new_owner: AccountId) -> bool {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            "Owner's method"
        );
        require!(!new_owner.as_str().is_empty(), "New owner cannot be empty");
        log!("Owner updated from {} to {}", self.owner_id, new_owner);
        self.owner_id = new_owner;
        true
    }

    /// Set stake amount (only callable by the owner).
    /// - `stake_amount`: Amount required to stake.
    #[payable]
    pub fn set_stake_amount(&mut self, stake_amount: U128) {
        assert_one_yocto();
        assert_eq!(
            self.owner_id,
            env::predecessor_account_id(),
            "Only the owner can set stake amount."
        );
        let amount = stake_amount.0;
        assert!(amount > 0, "Amount should gt 0.");
        self.stake_amount = amount;
        env::log_str(&format!("Stake amount updated to {}", self.stake_amount));
    }

    /// Unstake all principal
    #[payable]
    pub fn unstake(&mut self) -> u128 {
        assert_one_yocto();
        let account_id = env::predecessor_account_id();
        let stake_info = self
            .staked_balances
            .get(&account_id)
            .expect("No stake found for this account");

        match self.user_states.get(&account_id) {
            Some(UserOperationState::Idle) | None => {
                // pass
                self.user_states
                    .insert(&account_id, &UserOperationState::Unstaking);
                env::log_str("Unstake operation started.");
            }
            Some(UserOperationState::Staking) => {
                env::panic_str("Cannot unstake while staking is in progress.");
            }
            Some(UserOperationState::Unstaking) => {
                env::panic_str("Unstake operation already in progress.");
            }
        }
        // Calculate the time difference and accumulated rewards
        let current_time = env::block_timestamp() / NANOSECONDS; // Convert nanoseconds to seconds
        require!(
            current_time >= stake_info.start_time + self.lock_duration,
            "It is not yet time to unstake."
        );

        let total_payout = stake_info.amount;

        // Remove staking record
        self.staked_balances.remove(&account_id);

        // Transfer principal and rewards to the user
        Promise::new(self.token_contract.clone())
            .function_call(
                "ft_transfer".to_string(),
                serde_json::json!({
                    "receiver_id": account_id,
                    "amount": total_payout.to_string(),
                })
                .to_string()
                .into_bytes(),
                NearToken::from_yoctonear(1), // Attach 1 yoctoNEAR
                Gas::from_gas(20_000_000_000_000),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_gas(5_000_000_000_000))
                    .on_ft_transfer_then_remove(
                        account_id,
                        stake_info.amount,
                        stake_info.start_time,
                    ),
            );
        total_payout
    }

    /// Callback: After ft_transfer, only then remove staking record.
    #[private]
    pub fn on_ft_transfer_then_remove(
        &mut self,
        account_id: AccountId,
        stake_amount: u128,
        start_time: u64,
        #[callback_result] call_result: Result<(), near_sdk::PromiseError>,
    ) -> bool {
        match call_result {
            Ok(()) => {
                self.total_staked -= stake_amount;
                self.total_user -= 1;
                self.user_states
                    .insert(&account_id, &UserOperationState::Idle);
                true
            }
            Err(_) => {
                let stake_info = StakeInfo {
                    amount: stake_amount,
                    start_time,
                };
                self.staked_balances.insert(&account_id, &stake_info);
                self.user_states
                    .insert(&account_id, &UserOperationState::Idle);
                false
            }
        }
    }

    /// Query staking information for a specific user
    pub fn get_stake_info(&self, account_id: AccountId) -> Option<StakeInfo> {
        self.staked_balances.get(&account_id)
    }

    /// Query total stake
    pub fn get_total_stake(&self) -> u128 {
        self.total_staked
    }
    /// Query total stake user
    pub fn get_total_user(&self) -> u64 {
        self.total_user
    }
    /// Query total amount of stake
    pub fn get_stake_amount(&self) -> u128 {
        self.stake_amount
    }

    /// Query owner
    pub fn owner(&self) -> AccountId {
        self.owner_id.clone()
    }

    /// Query lock duration
    pub fn get_lock_duration(&self) -> u64 {
        self.lock_duration
    }

    /// User staked or not.
    pub fn user_staked(&self, account_id: AccountId) -> UserStakeInfo {
        let mut user_stake_info = UserStakeInfo {
            staked: false,
            amount: 0,
            start_time: 0,
        };
        if let Some(stake_info) = self.staked_balances.get(&account_id) {
            user_stake_info.staked = stake_info.amount >= self.stake_amount;
            user_stake_info.amount = stake_info.amount;
            user_stake_info.start_time = stake_info.start_time;
        }
        user_stake_info
    }

    pub fn search_stake_infos(
        &self,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Vec<(AccountId, StakeInfo)> {
        let start = offset.unwrap_or(0);
        let l = limit.unwrap_or(50);
        self.staked_balances
            .iter()
            .skip(start as usize)
            .take(l as usize)
            .collect()
    }
}

/// Implementation of NEP-141 `ft_on_transfer` method
#[near]
impl FungibleTokenReceiver for StakingContract {
    /// Handle token transfers for staking
    fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        // Ensure that the token being transferred is the one specified in the contract
        assert_eq!(
            env::predecessor_account_id(),
            self.token_contract,
            "Only the specified token can be staked"
        );

        assert_eq!(self.stake_paused, false, "Stake paused");

        match self.user_states.get(&sender_id) {
            Some(UserOperationState::Idle) | None => {
                self.user_states
                    .insert(&sender_id, &UserOperationState::Staking);
                env::log_str("Stake operation started.");
            }
            Some(UserOperationState::Staking) => {
                env::panic_str("Stake operation already in progress.");
            }
            Some(UserOperationState::Unstaking) => {
                env::panic_str("Cannot stake while unstake is in progress.");
            }
        }
        // Get the current timestamp
        let current_time = env::block_timestamp() / NANOSECONDS; // Convert nanoseconds to seconds

        // Update or create the user's staking record
        let mut stake_info = self.staked_balances.get(&sender_id).unwrap_or(StakeInfo {
            amount: 0,
            start_time: current_time,
        });

        // Update principal and timestamp
        let base_amount = stake_info.amount;
        let inc_amount = amount.0;
        let stake_amount = self.stake_amount;

        require!(
            base_amount + inc_amount == stake_amount,
            "You need to stake an appropriate amount."
        );
        if base_amount == 0 {
            self.total_user += 1;
        }

        stake_info.amount += inc_amount;
        stake_info.start_time = current_time;

        self.staked_balances.insert(&sender_id, &stake_info);

        self.total_staked += inc_amount;

        self.user_states
            .insert(&sender_id, &UserOperationState::Idle);
        // Return 0 to indicate the transfer was successfully handled
        PromiseOrValue::Value(U128(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::json_types::U128;
    use near_sdk::test_utils::accounts;
    use near_sdk::{test_utils::VMContextBuilder, testing_env, AccountId};

    const TOKEN_CONTRACT: &str = "token.testnet";

    /// Helper function to create a mock context
    fn get_context(
        predecessor: AccountId,
        attached_deposit: u128,
        block_timestamp: u64,
    ) -> VMContextBuilder {
        let mut builder = VMContextBuilder::new();
        builder
            .predecessor_account_id(predecessor) // The account that sends the call (e.g., the token contract)
            .attached_deposit(NearToken::from_yoctonear(attached_deposit)) // The deposit attached with the call
            .block_timestamp(block_timestamp); // Set the block timestamp
        builder
    }

    #[test]
    fn test_contract_initialization() {
        // Set up the testing environment
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, 0);
        testing_env!(context.build());

        // Initialize the contract
        let token_contract: AccountId = TOKEN_CONTRACT.parse().unwrap();
        let contract = StakingContract::new(accounts(0), token_contract.clone());

        // Check initialization
        assert_eq!(contract.owner_id, accounts(0));
        assert_eq!(contract.token_contract, token_contract);
    }

    #[test]
    fn test_staking() {
        // Set up the testing environment
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, 0);
        testing_env!(context.build());

        // Initialize the contract
        let mut contract = StakingContract::new(accounts(0), TOKEN_CONTRACT.parse().unwrap());

        // Simulate a user staking tokens via ft_on_transfer
        let sender_id = accounts(1);
        let stake_amount = U128(100_000_000_000_000_000_000);

        contract.ft_on_transfer(sender_id.clone(), stake_amount, "".to_string());

        // Check if the user's staking record is updated
        let stake_info = contract.get_stake_info(sender_id).unwrap();
        assert_eq!(stake_info.amount, stake_amount.0);
    }

    #[test]
    fn test_multiple_staking() {
        // Set up the testing environment
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, 0);
        testing_env!(context.build());

        // Initialize the contract
        let mut contract = StakingContract::new(accounts(0), TOKEN_CONTRACT.parse().unwrap());

        // Simulate a user staking tokens multiple times
        let sender_id = accounts(1);
        let first_stake_amount = U128(100_000_000_000_000_000_000);
        let second_stake_amount = U128(100_000_000_000_000_000_000);

        contract.ft_on_transfer(sender_id.clone(), first_stake_amount, "".to_string());
        let context = get_context(accounts(0), 1, 0);
        testing_env!(context.build());
        contract.set_stake_amount(U128(200_000_000_000_000_000_000));
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, 0);
        testing_env!(context.build());
        contract.ft_on_transfer(sender_id.clone(), second_stake_amount, "".to_string());

        // Check if the user's staking record is updated
        let stake_info = contract.get_stake_info(sender_id).unwrap();
        assert_eq!(
            stake_info.amount,
            first_stake_amount.0 + second_stake_amount.0
        );
        assert_eq!(contract.get_total_user(), 1);
        assert_eq!(contract.get_total_stake(), 200_000_000_000_000_000_000);
    }

    #[test]
    fn test_staking_longtime() {
        // Set up the testing environment
        let initial_timestamp = 0;
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, initial_timestamp);
        testing_env!(context.build());

        // Initialize the contract
        let mut contract = StakingContract::new(accounts(0), TOKEN_CONTRACT.parse().unwrap());
        let sender_id = accounts(1);
        let stake_amount = U128(100_000_000_000_000_000_000);
        contract.ft_on_transfer(sender_id.clone(), stake_amount, "".to_string());
        // Simulate time passing (1 year)
        let new_timestamp = initial_timestamp + 365 * 24 * 60 * 60 * 1_000_000_000;
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, new_timestamp);
        testing_env!(context.build());

        // Get stake info with real-time rewards
        let stake_info = contract.get_stake_info(sender_id).unwrap();

        // Assert that the accumulated reward matches the expected rewards
        assert_eq!(stake_info.amount, 100_000_000_000_000_000_000);
    }

    #[test]
    fn test_unstaking() {
        // Set up the testing environment
        let initial_timestamp = 0;
        let context = get_context(TOKEN_CONTRACT.parse().unwrap(), 0, initial_timestamp);
        testing_env!(context.build());

        // Initialize the contract
        let mut contract = StakingContract::new(accounts(0), TOKEN_CONTRACT.parse().unwrap());

        // Simulate a user staking tokens
        let sender_id = accounts(1);
        let stake_amount = U128(100_000_000_000_000_000_000);
        contract.ft_on_transfer(sender_id.clone(), stake_amount, "".to_string());

        // Simulate time passing (1 year)
        let new_timestamp = initial_timestamp + 365 * 24 * 60 * 60 * 1_000_000_000; // Add 1 year in nanoseconds
        let context = get_context(accounts(1), 1, new_timestamp);
        testing_env!(context.build());

        let mut stake_info = contract.get_stake_info(sender_id.clone());
        // Unstake all tokens
        contract.unstake();
        let stake = stake_info.unwrap();
        contract.on_ft_transfer_then_remove(accounts(1), stake.amount, stake.start_time, Ok(()));
        // Check that the user's staking record is removed
        stake_info = contract.get_stake_info(sender_id);
        assert!(stake_info.is_none());
        assert_eq!(contract.get_total_stake(), 0);
        assert_eq!(contract.get_total_user(), 0);
    }
}
