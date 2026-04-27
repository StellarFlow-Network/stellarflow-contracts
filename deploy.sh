#!/bin/bash

# StellarFlow Time-Locked Upgrade Contract Deployment Script

set -e

echo "🚀 Deploying StellarFlow Time-Locked Upgrade Contract..."

# Check if Rust and Cargo are installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Cargo not found. Please install Rust first:"
    echo "   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

# Check if Stellar CLI is installed
if ! command -v stellar &> /dev/null; then
    echo "❌ Stellar CLI not found. Please install it first:"
    echo "   cargo install stellar-cli --locked"
    exit 1
fi

# Build the contract
echo "🔨 Building contract..."
cargo build --target wasm32-unknown-unknown --release

# Check if build was successful
if [ ! -f "target/wasm32-unknown-unknown/release/stellarflow_contracts.wasm" ]; then
    echo "❌ Build failed. Please check the errors above."
    exit 1
fi

echo "✅ Build successful!"

# Get the WASM hash for verification
WASM_HASH=$(sha256sum target/wasm32-unknown-unknown/release/stellarflow_contracts.wasm | cut -d' ' -f1)
echo "📋 WASM Hash: $WASM_HASH"

# Deploy contract (uncomment and modify with your network details)
# echo "🌐 Deploying to network..."
# stellar contract deploy \
#   --wasm target/wasm32-unknown-unknown/release/stellarflow_contracts.wasm \
#   --source <YOUR_ACCOUNT> \
#   --network <NETWORK_NAME>

echo "🎉 Contract ready for deployment!"
echo ""
echo "Next steps:"
echo "1. Set up your Stellar network configuration"
echo "2. Uncomment and modify the deployment command above"
echo "3. Run: ./deploy.sh"
echo ""
echo "Usage after deployment:"
echo "- Initialize: stellar contract invoke --id <CONTRACT_ID> -- initialize --admin <ADMIN_ADDRESS>"
echo "- Propose upgrade: stellar contract invoke --id <CONTRACT_ID> -- propose_upgrade --new_wasm_hash <HASH> --proposer <ADMIN_ADDRESS>"
echo "- Check timelock: stellar contract invoke --id <CONTRACT_ID> -- get_upgrade_timelock_remaining"
echo "- Execute upgrade: stellar contract invoke --id <CONTRACT_ID> -- execute_upgrade --executor <ADMIN_ADDRESS>"
