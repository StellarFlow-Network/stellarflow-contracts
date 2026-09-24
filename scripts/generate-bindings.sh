#!/usr/bin/env bash
# =============================================================================
# generate-bindings.sh — Automated TypeScript Contract Client Package Builder
# Issue: #635 | stellarflow-contracts
#
# Invokes `stellar contract bindings typescript` for every workspace contract
# WASM and emits a typed TS package per contract into the configured output
# directory, ready for consumption by stellarflow-frontend.
#
# Usage:
#   ./scripts/generate-bindings.sh [OPTIONS]
#
# Options:
#   --network   <network>   Soroban network alias (default: testnet)
#   --rpc-url   <url>       Override the Soroban RPC URL
#   --output    <dir>       Root directory for generated packages
#                           (default: packages/)
#   --contracts <list>      Comma-separated list of contract names to process.
#                           Omit to process every discovered contract.
#   --help                  Show this help message and exit.
#
# Environment variables (all optional, lower precedence than flags):
#   STELLAR_NETWORK         — same as --network
#   STELLAR_RPC_URL         — same as --rpc-url
#   BINDINGS_OUTPUT_DIR     — same as --output
#
# Requirements:
#   • stellar CLI ≥ v21  (https://developers.stellar.org/docs/tools/developer-tools/cli/install-cli)
#   • Rust / cargo build tool-chain (for `cargo build --release`)
#   • node / npm (for package.json scaffolding inside each generated package)
# =============================================================================

set -euo pipefail

# ── Colour helpers ─────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()    { echo -e "${CYAN}[INFO]${RESET}  $*"; }
success() { echo -e "${GREEN}[OK]${RESET}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${RESET}  $*"; }
error()   { echo -e "${RED}[ERROR]${RESET} $*" >&2; }
die()     { error "$*"; exit 1; }

# ── Defaults ───────────────────────────────────────────────────────────────
NETWORK="${STELLAR_NETWORK:-testnet}"
RPC_URL="${STELLAR_RPC_URL:-}"
OUTPUT_DIR="${BINDINGS_OUTPUT_DIR:-packages}"
FILTER_CONTRACTS=""
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_DIR="${REPO_ROOT}/target/wasm32-unknown-unknown/release"

# ── Parse CLI flags ────────────────────────────────────────────────────────
print_help() {
  sed -n '2,35p' "${BASH_SOURCE[0]}"
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --network)   NETWORK="$2";           shift 2 ;;
    --rpc-url)   RPC_URL="$2";           shift 2 ;;
    --output)    OUTPUT_DIR="$2";        shift 2 ;;
    --contracts) FILTER_CONTRACTS="$2";  shift 2 ;;
    --help|-h)   print_help ;;
    *) die "Unknown option: $1. Run with --help for usage." ;;
  esac
done

# Resolve output dir relative to repo root when not absolute
[[ "${OUTPUT_DIR}" = /* ]] || OUTPUT_DIR="${REPO_ROOT}/${OUTPUT_DIR}"

# ── Preflight checks ───────────────────────────────────────────────────────
info "Checking required tools..."

command -v stellar &>/dev/null \
  || die "'stellar' CLI not found. Install from: https://developers.stellar.org/docs/tools/developer-tools/cli/install-cli"

STELLAR_VERSION="$(stellar --version 2>&1 | head -n1)"
info "stellar CLI: ${STELLAR_VERSION}"

command -v cargo &>/dev/null \
  || die "'cargo' not found. Install Rust from: https://rustup.rs"

# ── Discover workspace contracts ───────────────────────────────────────────
# Each workspace member in contracts/ that produces a cdylib is a candidate.
discover_contracts() {
  local contracts_dir="${REPO_ROOT}/contracts"
  local found=()

  for manifest in "${contracts_dir}"/*/Cargo.toml; do
    local dir; dir="$(dirname "${manifest}")"
    local name; name="$(basename "${dir}")"

    # Only include members that build a cdylib (i.e., a deployable contract)
    if grep -q 'cdylib' "${manifest}" 2>/dev/null; then
      found+=("${name}")
    fi
  done

  echo "${found[@]:-}"
}

# ── Build contracts ────────────────────────────────────────────────────────
build_contracts() {
  info "Building all workspace contracts in release mode..."
  (
    cd "${REPO_ROOT}"
    cargo build \
      --release \
      --target wasm32-unknown-unknown \
      --workspace \
      --exclude stellarflow-contracts \
      2>&1
  ) && success "Build complete." || die "Cargo build failed — fix compilation errors first."
}

# ── Generate bindings for a single contract ────────────────────────────────
generate_for_contract() {
  local contract_name="$1"
  # stellar CLI expects hyphen-separated names to be underscore in WASM filename
  local wasm_name="${contract_name//-/_}"
  local wasm_path="${WASM_DIR}/${wasm_name}.wasm"

  if [[ ! -f "${wasm_path}" ]]; then
    warn "WASM not found for '${contract_name}' at ${wasm_path}. Skipping."
    return 0
  fi

  local pkg_dir="${OUTPUT_DIR}/${contract_name}"
  mkdir -p "${pkg_dir}"

  info "Generating TypeScript bindings for '${contract_name}'..."

  # Build the stellar CLI command
  local cmd=(
    stellar contract bindings typescript
    --wasm    "${wasm_path}"
    --output-dir "${pkg_dir}"
    --overwrite
  )

  # Append network/rpc flags when provided
  [[ -n "${NETWORK}" ]]  && cmd+=(--network  "${NETWORK}")
  [[ -n "${RPC_URL}" ]]  && cmd+=(--rpc-url  "${RPC_URL}")

  if "${cmd[@]}" 2>&1; then
    success "  → ${contract_name}: bindings written to ${pkg_dir}"
  else
    warn "  → ${contract_name}: stellar CLI returned non-zero. Check WASM validity."
    return 1
  fi
}

# ── Patch package.json name field ─────────────────────────────────────────
# stellar CLI generates a generic package.json; we scope it under @stellarflow.
patch_package_json() {
  local contract_name="$1"
  local pkg_json="${OUTPUT_DIR}/${contract_name}/package.json"

  if [[ ! -f "${pkg_json}" ]]; then
    return 0
  fi

  local scoped_name="@stellarflow/${contract_name}"

  # Use node if available for robust JSON editing; fall back to sed
  if command -v node &>/dev/null; then
    node - "${pkg_json}" "${scoped_name}" <<'NODE_EOF'
const fs   = require('fs');
const path = process.argv[2];
const name = process.argv[3];
const pkg  = JSON.parse(fs.readFileSync(path, 'utf8'));
pkg.name   = name;
pkg.repository = {
  type: 'git',
  url:  'https://github.com/StellarFlow-Network/stellarflow-contracts',
  directory: `packages/${name.split('/')[1]}`,
};
pkg.keywords = [...(pkg.keywords || []), 'stellarflow', 'soroban', 'stellar'];
fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + '\n');
console.log(`Patched package.json name → ${name}`);
NODE_EOF
  else
    # Best-effort sed patch (works for simple cases)
    sed -i "s|\"name\":[[:space:]]*\"[^\"]*\"|\"name\": \"${scoped_name}\"|" "${pkg_json}"
    warn "node not found — used sed to patch package.json (may be imprecise)."
  fi
}

# ── Main ───────────────────────────────────────────────────────────────────
main() {
  echo -e "${BOLD}"
  echo "════════════════════════════════════════════════════════════"
  echo "  StellarFlow — TypeScript Contract Bindings Generator"
  echo "  Issue #635"
  echo "════════════════════════════════════════════════════════════"
  echo -e "${RESET}"

  info "Repository root : ${REPO_ROOT}"
  info "Output directory: ${OUTPUT_DIR}"
  info "Network         : ${NETWORK:-<none>}"

  # Determine contracts to process
  local all_contracts
  mapfile -t all_contracts < <(discover_contracts | tr ' ' '\n')

  local targets=()
  if [[ -n "${FILTER_CONTRACTS}" ]]; then
    IFS=',' read -ra targets <<< "${FILTER_CONTRACTS}"
  else
    targets=("${all_contracts[@]}")
  fi

  if [[ ${#targets[@]} -eq 0 ]]; then
    die "No deployable contracts found under contracts/. Nothing to generate."
  fi

  info "Contracts to process: ${targets[*]}"

  # Build step
  build_contracts

  # Generation loop
  local ok=0 fail=0
  mkdir -p "${OUTPUT_DIR}"

  for contract in "${targets[@]}"; do
    if generate_for_contract "${contract}"; then
      patch_package_json "${contract}"
      (( ok++ )) || true
    else
      (( fail++ )) || true
    fi
  done

  echo ""
  echo -e "${BOLD}════ Summary ════${RESET}"
  success "Generated : ${ok} package(s)"
  [[ ${fail} -gt 0 ]] && warn "Skipped / failed: ${fail} contract(s)"

  echo ""
  info "Next steps:"
  echo "  1. Review generated packages in: ${OUTPUT_DIR}/"
  echo "  2. In each package run: npm install && npm run build"
  echo "  3. Publish or link into stellarflow-frontend via:"
  echo "       npm install ${OUTPUT_DIR}/<contract-name>"
  echo "     or add to frontend package.json as a workspace dependency."
  echo ""

  [[ ${fail} -gt 0 ]] && exit 1 || exit 0
}

main "$@"
