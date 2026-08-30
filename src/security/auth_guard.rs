use soroban_sdk::{Env, Address, Symbol, Vec, Val, auth::{InvokerContractAuthEntry, SubContractInvocation, ContractContext}};
use crate::ContractError;

pub struct AuthContextGuard;

impl AuthContextGuard {
    pub fn enforce_isolation(env: &Env, expected_caller: &Address) -> Result<(), ContractError> {
        let invoking_contract = env.current_contract_address();
        let previous_context = env.auths();
        for auth_entry in previous_context.iter() {
            if auth_entry.address != *expected_caller {
                continue;
            }
            if auth_entry.context.contract != invoking_contract {
                return Err(ContractError::UnauthorizedReentryAttempt);
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
