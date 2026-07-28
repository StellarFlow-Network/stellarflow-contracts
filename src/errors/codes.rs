#![no_std]

use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ErrorCode {
    AlreadyInitialized = 1001,
    NotInitialized = 1002,
    ContractPaused = 1003,
    Unauthorized = 2001,
    InvalidSignature = 2002,
    AdminNotFound = 2003,
    NotAuthorizedProvider = 2004,
    InvalidArgument = 3001,
    InvalidAsset = 3002,
    AmountTooSmall = 3003,
    InvalidPriceBounds = 3004,
    InvalidProviderWeight = 3005,
    InsufficientBalance = 4001,
    InsufficientLiquidity = 4002,
    SlippageExceeded = 4003,
    FlashLoanProtectionTriggered = 4004,
    OracleStaleData = 5001,
    OracleFlashCrash = 5002,
    OracleDataMissing = 5003,
    ReentrancyDetected = 6001,
    CapacityExceeded = 6002,
    StorageError = 6003,
}