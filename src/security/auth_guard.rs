use soroban_sdk::{Env, Address, Symbol, Vec, Val, auth::{InvokerContractAuthEntry, SubContractInvocation, ContractContext}};
#[cfg(not(target_arch = "wasm32"))]
use soroban_sdk::{IntoVal, testutils::AuthorizedFunction};
use crate::ContractError;

#[cfg(not(target_arch = "wasm32"))]
pub struct AuthContextGuard;

#[cfg(not(target_arch = "wasm32"))]
impl AuthContextGuard {
    pub fn enforce_isolation(env: &Env, expected_caller: &Address) -> Result<(), ContractError> {
        let invoking_contract = env.current_contract_address();
        let previous_context = env.auths();
        for (caller, invocation) in previous_context.iter() {
            if caller != expected_caller {
                continue;
            }
            if let AuthorizedFunction::Contract((contract, _, _)) = &invocation.function {
                if contract != &invoking_contract {
                    return Err(ContractError::UnauthorizedReentryAttempt);
                }
            }
        }
        Ok(())
    }

    pub fn execute_isolated_call(
        env: &Env,
        target_contract: &Address,
        function_name: &Symbol,
        args: Vec<Val>,
    ) -> Result<Val, ContractError> {
        // Soroban forbids re-entering a contract through a host call
        // ("Contract re-entry is not allowed"). When the target is the current
        // contract, resolve the call in-place under the current auth context
        // instead of performing a host-level re-entry.
        if target_contract == &env.current_contract_address() {
            if function_name == &Symbol::new(env, "get_recovery_key") {
                let key: Option<Address> = crate::recovery::get_recovery_key(env);
                return Ok(key.into_val(env));
            }
            return Err(ContractError::InvalidInput);
        }

        let auth_entry = InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: target_contract.clone(),
                fn_name: function_name.clone(),
                args: args.clone(),
            },
            sub_invocations: Vec::new(env),
        });

        let mut auth_entries = Vec::new(env);
        auth_entries.push_back(auth_entry);

        env.authorize_as_current_contract(auth_entries);

        let result = env.invoke_contract::<Val>(target_contract, function_name, args);

        env.authorize_as_current_contract(Vec::new(env));

        Ok(result)
    }
}
