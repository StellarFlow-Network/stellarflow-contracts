//! Soroban call-stack depth and recursion boundary coverage.
//!
//! The host charges every cross-contract invocation a frame on its context
//! stack. `soroban-env-host` refuses the invocation once that stack reaches
//! `DEFAULT_HOST_DEPTH_LIMIT` frames, and refuses any invocation of a contract
//! that is *already* on the stack (`ContractReentryMode::Prohibited`). Neither
//! boundary may surface as a native stack overflow.
//!
//! These tests walk a chain of distinct contracts to ten levels, confirm the
//! budget the host charges for it, and drive the re-entry boundary to confirm
//! it fails closed with a typed host error. The depth ceiling itself is driven
//! from below and never tripped: exceeding it surfaces as an uncatchable host
//! abort inside the `testutils` harness, which would take the whole test binary
//! down instead of failing one test.

use soroban_sdk::{
    contract, contractimpl, contracttype,
    symbol_short, vec,
    xdr::{ScErrorCode, ScErrorType},
    Address, Env, IntoVal,
};

/// Storage keys shared by the probe contracts.
#[contracttype]
pub enum ProbeKey {
    /// Successor this link invokes.
    Next,
    /// Set once this contract's entrypoint has been entered.
    Entered,
    /// Set when this contract caught a `Context` host error from its callee.
    CaughtContextError,
    /// Set when this contract caught `ScErrorCode::ExceededLimit`.
    CaughtExceededLimit,
    /// Set when this contract caught `ScErrorCode::InvalidAction`.
    CaughtInvalidAction,
}

/// The depth the acceptance criteria require to work.
const REQUIRED_DEPTH: u32 = 10;

/// A depth well past the required one, still well inside the host's 100-frame
/// ceiling, so the suite pins real headroom above `REQUIRED_DEPTH`.
const HEADROOM_DEPTH: u32 = 32;

/// Records the shape of a host error caught from a callee and returns the
/// sentinel a refusing link reports.
fn catch(env: &Env, err: soroban_sdk::Error) -> u32 {
    env.storage()
        .instance()
        .set(&ProbeKey::CaughtContextError, &err.is_type(ScErrorType::Context));
    env.storage()
        .instance()
        .set(&ProbeKey::CaughtExceededLimit, &err.is_code(ScErrorCode::ExceededLimit));
    env.storage()
        .instance()
        .set(&ProbeKey::CaughtInvalidAction, &err.is_code(ScErrorCode::InvalidAction));
    u32::MAX
}

/// One link in a cross-contract call chain. Each link holds its own successor,
/// so a chain is a sequence of *distinct* contracts rather than a re-entrant
/// call into one.
#[contract]
pub struct DepthLink;

#[contractimpl]
impl DepthLink {
    /// Point this link at the next contract in the chain.
    pub fn set_next(env: Env, next: Address) {
        env.storage().instance().set(&ProbeKey::Next, &next);
    }

    /// Whether this link's `walk` has been entered.
    pub fn entered(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::Entered)
            .unwrap_or(false)
    }

    /// Whether this link caught a `Context` host error.
    pub fn caught_context_error(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::CaughtContextError)
            .unwrap_or(false)
    }

    /// Whether this link caught `ScErrorCode::ExceededLimit`.
    pub fn caught_exceeded_limit(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::CaughtExceededLimit)
            .unwrap_or(false)
    }

    /// Whether this link caught `ScErrorCode::InvalidAction`.
    pub fn caught_invalid_action(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::CaughtInvalidAction)
            .unwrap_or(false)
    }

    /// Enter this link, then `remaining` further links, returning how many
    /// links were entered. Returns `u32::MAX` when a callee is refused, after
    /// recording the shape of the refusal, so a host boundary stays observable
    /// as a value rather than as a panicking contract call.
    pub fn walk(env: Env, remaining: u32) -> u32 {
        env.storage().instance().set(&ProbeKey::Entered, &true);

        if remaining == 0 {
            return 1;
        }

        let next: Address = env
            .storage()
            .instance()
            .get(&ProbeKey::Next)
            .expect("a non-terminal link must have a successor");

        match env.try_invoke_contract::<u32, soroban_sdk::Error>(
            &next,
            &symbol_short!("walk"),
            vec![&env, (remaining - 1).into_val(&env)],
        ) {
            Ok(Ok(reached)) => reached.saturating_add(1),
            Ok(Err(_)) => u32::MAX,
            Err(Ok(err)) => catch(&env, err),
            Err(Err(_)) => u32::MAX,
        }
    }
}

/// A contract that tries to call back into itself.
#[contract]
pub struct SelfCaller;

#[contractimpl]
impl SelfCaller {
    /// Whether this contract's `recurse` has been entered.
    pub fn self_entered(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::Entered)
            .unwrap_or(false)
    }

    /// Whether this contract caught a `Context` host error.
    pub fn self_caught_context_error(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::CaughtContextError)
            .unwrap_or(false)
    }

    /// Whether this contract caught `ScErrorCode::InvalidAction`.
    pub fn self_caught_invalid_action(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ProbeKey::CaughtInvalidAction)
            .unwrap_or(false)
    }

    /// Attempt `depth` nested calls into this same contract.
    pub fn recurse(env: Env, depth: u32) -> u32 {
        env.storage().instance().set(&ProbeKey::Entered, &true);

        if depth == 0 {
            return 1;
        }

        let me = env.current_contract_address();
        match env.try_invoke_contract::<u32, soroban_sdk::Error>(
            &me,
            &symbol_short!("recurse"),
            vec![&env, (depth - 1).into_val(&env)],
        ) {
            Ok(Ok(reached)) => reached.saturating_add(1),
            Ok(Err(_)) => u32::MAX,
            Err(Ok(err)) => catch(&env, err),
            Err(Err(_)) => u32::MAX,
        }
    }
}

/// Register `len` links. `link(i)` points at `link(i + 1)` when `chain` is set,
/// and at `link((i + 1) % len)` when it is not, forming a cycle. Returns the
/// head link followed by every link in order.
fn build_universe(env: &Env, len: u32, chain: bool) -> (Address, soroban_sdk::Vec<Address>) {
    assert!(len > 0, "a walk needs at least one link");

    let mut links = soroban_sdk::Vec::new(env);
    for _ in 0..len {
        links.push_back(env.register_contract(None, DepthLink));
    }
    for i in 0..len {
        // The final link of a chain is terminal and is never asked for a
        // successor, so it is left unwired.
        let next = if chain {
            if i + 1 == len {
                continue;
            }
            links.get(i + 1).unwrap()
        } else {
            links.get((i + 1) % len).unwrap()
        };
        DepthLinkClient::new(env, &links.get(i).unwrap()).set_next(&next);
    }
    (links.get(0).unwrap(), links)
}

/// `REQUIRED_DEPTH - 1` hops from the head enter `REQUIRED_DEPTH` links and
/// return `REQUIRED_DEPTH`. This is the acceptance criterion: a ten-deep
/// cross-contract call chain executes.
#[test]
fn chain_of_ten_cross_contract_calls_executes() {
    let env = Env::default();
    let (head, links) = build_universe(&env, REQUIRED_DEPTH, true);

    assert_eq!(
        DepthLinkClient::new(&env, &head).walk(&(REQUIRED_DEPTH - 1)),
        REQUIRED_DEPTH,
        "a {REQUIRED_DEPTH}-deep chain must execute"
    );

    // Every link was actually entered, so the chain is genuinely deep rather
    // than short-circuiting somewhere in the middle.
    for i in 0..REQUIRED_DEPTH {
        assert!(
            DepthLinkClient::new(&env, &links.get(i).unwrap()).entered(),
            "link {i} was never entered"
        );
    }
}

/// A chain `HEADROOM_DEPTH` deep completes on the host's default budget, and
/// the host charges a bounded, non-zero amount of CPU and memory for a deep
/// chain. A native stack overflow would abort the test binary, so reaching the
/// assertions at all is part of the check.
#[test]
fn deep_chain_stays_within_the_host_recursion_budget() {
    let env = Env::default();
    let (head, _) = build_universe(&env, HEADROOM_DEPTH, true);
    let client = DepthLinkClient::new(&env, &head);

    // A single link, for a baseline on this same host.
    let shallow_cpu_before = env.budget().cpu_instruction_cost();
    let shallow_mem_before = env.budget().memory_bytes_cost();
    assert_eq!(client.walk(&0), 1);
    let shallow_cpu = env.budget().cpu_instruction_cost() - shallow_cpu_before;
    let shallow_mem = env.budget().memory_bytes_cost() - shallow_mem_before;

    // The full chain, measured on the same host so only the depth differs.
    let deep_cpu_before = env.budget().cpu_instruction_cost();
    let deep_mem_before = env.budget().memory_bytes_cost();
    assert_eq!(
        client.walk(&(HEADROOM_DEPTH - 1)),
        HEADROOM_DEPTH,
        "a {HEADROOM_DEPTH}-deep chain must execute"
    );
    let deep_cpu = env.budget().cpu_instruction_cost() - deep_cpu_before;
    let deep_mem = env.budget().memory_bytes_cost() - deep_mem_before;

    assert!(deep_cpu > 0, "the host must charge CPU for a deep chain");
    assert!(deep_mem > 0, "the host must charge memory for a deep chain");
    assert!(
        deep_cpu < 50_000_000,
        "a {HEADROOM_DEPTH}-deep chain must stay well inside the default CPU budget, got {deep_cpu}"
    );
    assert!(
        deep_mem < 5_000_000,
        "a {HEADROOM_DEPTH}-deep chain must stay well inside the default memory budget, got {deep_mem}"
    );

    // Budget grows with depth: the deep chain is strictly more expensive than
    // walking a single link.
    assert!(
        deep_cpu > shallow_cpu,
        "depth {HEADROOM_DEPTH} must cost more CPU than depth 1: {deep_cpu} vs {shallow_cpu}"
    );
    assert!(
        deep_mem > shallow_mem,
        "depth {HEADROOM_DEPTH} must cost more memory than depth 1: {deep_mem} vs {shallow_mem}"
    );
}

/// A contract already on the call stack cannot be entered again, so recursion
/// into a single contract is refused on the very first attempt with
/// `Context`/`InvalidAction`. An unbounded recursion can therefore never be
/// issued at all: the depth a caller can consume is bounded by the number of
/// distinct contracts it can reach.
#[test]
fn recursive_self_call_is_refused() {
    let env = Env::default();
    let id = env.register_contract(None, SelfCaller);
    let client = SelfCallerClient::new(&env, &id);

    assert_eq!(
        client.recurse(&REQUIRED_DEPTH),
        u32::MAX,
        "a self-call must be refused rather than recursed"
    );
    assert!(client.self_entered(), "the outer entrypoint ran");
    assert!(
        client.self_caught_context_error(),
        "the refusal is a Context host error"
    );
    assert!(
        client.self_caught_invalid_action(),
        "the refusal is Context/InvalidAction"
    );
}

/// Re-entry is refused even when the returning call comes from a *third*
/// contract, so a cycle of distinct contracts cannot recurse either: the link
/// that tries to close the cycle catches the refusal, and the links after it
/// are never entered again.
#[test]
fn recursion_through_a_cycle_is_refused() {
    const CYCLE: u32 = 3;

    let env = Env::default();
    let (head, links) = build_universe(&env, CYCLE, false);

    assert_eq!(
        DepthLinkClient::new(&env, &head).walk(&CYCLE),
        u32::MAX,
        "a cycle must not recurse"
    );

    let last = CYCLE - 1;
    let closer = DepthLinkClient::new(&env, &links.get(last).unwrap());
    assert!(
        closer.caught_context_error(),
        "the link closing the cycle must catch a Context host error"
    );
    assert!(
        closer.caught_invalid_action(),
        "the refusal is Context/InvalidAction"
    );
    assert!(
        !closer.caught_exceeded_limit(),
        "a cycle is refused before the depth ceiling is relevant"
    );

    for i in 0..CYCLE {
        let link = DepthLinkClient::new(&env, &links.get(i).unwrap());
        assert!(link.entered(), "link {i} was entered once");
        if i != last {
            assert!(
                !link.caught_context_error(),
                "link {i} is not the one the cycle was refused at"
            );
        }
    }
}
