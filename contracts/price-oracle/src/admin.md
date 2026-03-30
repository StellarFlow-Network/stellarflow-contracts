🦀 StellarFlow-Contracts
The High-Fidelity Oracle for African Corridors on Soroban.

StellarFlow is a decentralized data oracle built on the Stellar Network. It provides real-time, verified exchange rates for African currencies (NGN, KES, GHS) to the Soroban ecosystem, enabling the next generation of localized DeFi, cross-border payments, and yield protocols.

🏛️ Architecture Overview
The contract acts as a secure, authorized ledger for price data. It is designed to be:

Authorized: Only whitelisted providers (Relayers) can update prices.

Immutable: All updates are time-stamped and emitted as events for transparency.

Interoperable: Designed to be called by other Soroban smart contracts (C2C).

🚀 Getting Started
Prerequisites

Rust

Soroban CLI

Target: wasm32-unknown-unknown

Installation
Clone the repository:

git clone https://github.com/SFN/stellarflow-contracts.git
cd stellarflow-contracts
Build the contract:

soroban contract build 
📂 Project Structure

├── src/

| ├── lib.rs # Main contract entry point and public interface

│ ├── types.rs # Custom structs (PriceData) and Enums (DataKey)

│ ├── storage.rs # Persistent and Instance storage logic

│ ├── auth.rs # require_auth and Admin-check functions

│ └── test.rs # Comprehensive unit and integration tests

├── Cargo.toml # Project dependencies (soroban-sdk)

└── README.md

🛠️ Public Interface (API)
Admin Functions

initialize(admin: Address): Sets the global contract administrator.

add_provider(provider: Address): Whitelists a backend relayer to push data.

Data Submission (Authorized) update_price(source: Address, asset: Symbol, price: i128): Updates the price for a specific asset. Requires source authorization.

Data Retrieval (Public)

get_price(asset: Symbol) -> PriceData: Returns the latest price, timestamp, and provider info.

get_all_assets() -> Vec: Returns a list of all currently tracked currency pairs.

🧪 Testing Policy
We maintain a "No Test, No Merge" policy.

All Pull Requests (PRs) must include a updated test.rs file. To run the test suite:

cargo test
Tests must verify:

Success Paths: Correct data storage and retrieval.

Security Paths: Rejection of unauthorized update_price calls.

Edge Cases: Handling of missing assets or zero-value inputs.

📜 License
This project is licensed under the MIT License - see the LICENSE file for details.