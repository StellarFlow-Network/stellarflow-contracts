#!/bin/bash
#
# Automated CLI deployment script for StellarFlow contracts.
#
# Builds a contract, deploys it to testnet/mainnet/futurenet via stellar-cli,
# optionally initializes it, and appends the resulting contract ID to a
# per-network manifest so deployment history is never overwritten.
#
# Usage:
#   scripts/deploy.sh --contract <name> --network <testnet|mainnet|futurenet> --source <identity> [options]
#
# Options:
#   -c, --contract <name>        Contract directory under contracts/ (required)
#   -n, --network <name>         testnet | mainnet | futurenet (required)
#   -s, --source <identity>      stellar-cli source account/identity (required unless --dry-run)
#   -i, --init-args <string>     Args appended to the initialize invocation, e.g. "--admin GABC..."
#   -f, --init-args-file <path>  File containing init args (defaults to scripts/init-args/<contract>.args)
#       --skip-init              Do not call initialize after deploy
#       --rpc-url <url>          Override the default RPC URL for the network
#       --passphrase <text>      Override the default network passphrase
#       --dry-run                Print the planned commands without executing them
#   -h, --help                   Show this help text

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CONTRACT=""
NETWORK=""
SOURCE=""
INIT_ARGS=""
INIT_ARGS_FILE=""
SKIP_INIT=false
RPC_URL_OVERRIDE=""
PASSPHRASE_OVERRIDE=""
DRY_RUN=false

usage() {
    sed -n '2,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -c|--contract) CONTRACT="$2"; shift 2 ;;
        -n|--network) NETWORK="$2"; shift 2 ;;
        -s|--source) SOURCE="$2"; shift 2 ;;
        -i|--init-args) INIT_ARGS="$2"; shift 2 ;;
        -f|--init-args-file) INIT_ARGS_FILE="$2"; shift 2 ;;
        --skip-init) SKIP_INIT=true; shift ;;
        --rpc-url) RPC_URL_OVERRIDE="$2"; shift 2 ;;
        --passphrase) PASSPHRASE_OVERRIDE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Error: unknown argument '$1'" >&2; usage; exit 1 ;;
    esac
done

if [[ -z "$CONTRACT" ]]; then
    echo "Error: --contract is required." >&2
    exit 1
fi

if [[ ! -d "$REPO_ROOT/contracts/$CONTRACT" ]]; then
    echo "Error: no contract found at contracts/$CONTRACT." >&2
    echo "Available contracts:" >&2
    ls "$REPO_ROOT/contracts" >&2
    exit 1
fi

case "$NETWORK" in
    testnet)
        DEFAULT_RPC_URL="https://soroban-testnet.stellar.org"
        DEFAULT_PASSPHRASE="Test SDF Network ; September 2015"
        ;;
    futurenet)
        DEFAULT_RPC_URL="https://rpc-futurenet.stellar.org"
        DEFAULT_PASSPHRASE="Test SDF Future Network ; October 2022"
        ;;
    mainnet)
        DEFAULT_RPC_URL=""
        DEFAULT_PASSPHRASE="Public Global Stellar Network ; September 2015"
        ;;
    *)
        echo "Error: --network must be one of testnet, mainnet, futurenet (got '$NETWORK')." >&2
        exit 1
        ;;
esac

RPC_URL="${RPC_URL_OVERRIDE:-$DEFAULT_RPC_URL}"
PASSPHRASE="${PASSPHRASE_OVERRIDE:-$DEFAULT_PASSPHRASE}"

if [[ "$NETWORK" == "mainnet" && -z "$RPC_URL" ]]; then
    echo "Error: mainnet has no bundled default RPC URL. Pass --rpc-url <your-provider-url>." >&2
    exit 1
fi

if [[ -z "$SOURCE" && "$DRY_RUN" == false ]]; then
    echo "Error: --source is required (the stellar-cli identity to deploy with)." >&2
    exit 1
fi

if [[ "$SKIP_INIT" == false && -z "$INIT_ARGS" ]]; then
    if [[ -n "$INIT_ARGS_FILE" ]]; then
        CANDIDATE_FILE="$INIT_ARGS_FILE"
    else
        CANDIDATE_FILE="$SCRIPT_DIR/init-args/$CONTRACT.args"
    fi
    if [[ -f "$CANDIDATE_FILE" ]]; then
        INIT_ARGS="$(grep -v '^\s*#' "$CANDIDATE_FILE" | tr '\n' ' ' | sed 's/[[:space:]]*$//')"
    elif [[ -n "$INIT_ARGS_FILE" ]]; then
        echo "Error: init args file '$INIT_ARGS_FILE' not found." >&2
        exit 1
    fi
fi

if [[ "$SKIP_INIT" == false && -z "$INIT_ARGS" ]]; then
    echo "Warning: no init args provided via --init-args, --init-args-file, or scripts/init-args/$CONTRACT.args." >&2
    echo "         Skipping the initialize call. Pass --skip-init to silence this warning." >&2
    SKIP_INIT=true
fi

PACKAGE_NAME="$CONTRACT"
WASM_NAME="$(echo "$PACKAGE_NAME" | tr '-' '_').wasm"
WASM_PATH="$REPO_ROOT/target/wasm32-unknown-unknown/release/$WASM_NAME"

echo "== StellarFlow deploy =="
echo "Contract:   $CONTRACT"
echo "Network:    $NETWORK"
echo "RPC URL:    $RPC_URL"
echo "Dry run:    $DRY_RUN"
echo ""

BUILD_CMD="cargo build --manifest-path \"$REPO_ROOT/contracts/$CONTRACT/Cargo.toml\" --target wasm32-unknown-unknown --release"

DEPLOY_CMD=(stellar contract deploy
    --wasm "$WASM_PATH"
    --source "$SOURCE"
    --rpc-url "$RPC_URL"
    --network-passphrase "$PASSPHRASE")

if [[ "$DRY_RUN" == true ]]; then
    echo "[dry-run] would build with:"
    echo "  $BUILD_CMD"
    echo "[dry-run] would deploy with:"
    printf '  %q ' "${DEPLOY_CMD[@]}"; echo
    if [[ "$SKIP_INIT" == false ]]; then
        echo "[dry-run] would initialize with:"
        echo "  stellar contract invoke --id <CONTRACT_ID> --source $SOURCE --rpc-url $RPC_URL --network-passphrase \"$PASSPHRASE\" -- initialize $INIT_ARGS"
    fi
    exit 0
fi

echo "Building $CONTRACT..."
eval "$BUILD_CMD"

if [[ ! -f "$WASM_PATH" ]]; then
    echo "Error: expected WASM artifact not found at $WASM_PATH." >&2
    exit 1
fi

WASM_HASH="$(sha256sum "$WASM_PATH" | cut -d' ' -f1)"
echo "WASM hash: $WASM_HASH"

echo "Deploying $CONTRACT to $NETWORK..."
CONTRACT_ID="$("${DEPLOY_CMD[@]}")"
echo "Deployed contract ID: $CONTRACT_ID"

if [[ "$SKIP_INIT" == false ]]; then
    echo "Initializing $CONTRACT..."
    # shellcheck disable=SC2086
    stellar contract invoke \
        --id "$CONTRACT_ID" \
        --source "$SOURCE" \
        --rpc-url "$RPC_URL" \
        --network-passphrase "$PASSPHRASE" \
        -- initialize $INIT_ARGS
    echo "Initialized."
fi

MANIFEST_DIR="$REPO_ROOT/deployments"
MANIFEST_FILE="$MANIFEST_DIR/$NETWORK.jsonl"
mkdir -p "$MANIFEST_DIR"

TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
printf '{"contract":"%s","contract_id":"%s","wasm_hash":"%s","source":"%s","network":"%s","initialized":%s,"timestamp":"%s"}\n' \
    "$CONTRACT" "$CONTRACT_ID" "$WASM_HASH" "$SOURCE" "$NETWORK" "$([[ "$SKIP_INIT" == false ]] && echo true || echo false)" "$TIMESTAMP" \
    >> "$MANIFEST_FILE"

echo "Recorded deployment in $MANIFEST_FILE"
echo "Done."
