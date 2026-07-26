# Bugfix Requirements Document

## Introduction

Several public entrypoints in the StellarFlow Soroban contracts accept `Vec` inputs with no length guard. In the Soroban execution model every heap allocation costs CPU instructions, so a caller who supplies a very large vector can push a transaction past the hard network budget limits (100M instructions / 40MB memory), causing an on-chain abort. The two affected sites are:

1. **price-oracle** — `get_prices` and `get_prices_with_status` accept a `Vec<Symbol>` of asset symbols with no upper bound, and `get_index_price` accepts a `Vec<AssetWeight>` component list with no upper bound. Both iterate the full vector before any early exit, making the CPU cost proportional to caller-supplied length.
2. **reward-splitter** — `add_recipient` allows recipients to accumulate in storage without a cap on the total list size, so the `distribute` function (which iterates all recipients) becomes proportionally more expensive as recipients are added.

The fix is to validate collection lengths at the entrypoint boundary and return `ContractError::OutOfBounds` (price-oracle) or `Error::OutOfBounds` (reward-splitter) before any iteration or storage modification begins.

---

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN `get_prices` is called with a `Vec<Symbol>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system iterates every element, consuming CPU instructions proportional to the caller-supplied length and potentially aborting the transaction with an on-chain budget panic.

1.2 WHEN `get_prices_with_status` is called with a `Vec<Symbol>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system iterates every element without an upfront length check, allowing budget exhaustion identical to clause 1.1.

1.3 WHEN `get_index_price` is called with a `Vec<AssetWeight>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system passes the uncapped vector into the index-price calculation, which iterates all components and may exhaust the CPU budget.

1.4 WHEN `add_recipient` is called and the stored recipient list already contains `MAX_RECIPIENTS` (50) entries THEN the system appends a new entry beyond the cap, so subsequent calls to `distribute` iterate more than 50 recipients and consume proportionally more CPU instructions, risking budget exhaustion.

### Expected Behavior (Correct)

2.1 WHEN `get_prices` is called with a `Vec<Symbol>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system SHALL return `ContractError::OutOfBounds` immediately, before iterating any element or accessing storage.

2.2 WHEN `get_prices_with_status` is called with a `Vec<Symbol>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system SHALL return `ContractError::OutOfBounds` immediately, before iterating any element or accessing storage.

2.3 WHEN `get_index_price` is called with a `Vec<AssetWeight>` whose length exceeds `MAX_PATH_HOPS` (5) THEN the system SHALL return `ContractError::OutOfBounds` immediately, before entering the index-price calculation loop.

2.4 WHEN `add_recipient` is called and the stored recipient list already contains `MAX_RECIPIENTS` (50) entries THEN the system SHALL return `Error::OutOfBounds` immediately, before appending the new recipient or updating storage.

### Unchanged Behavior (Regression Prevention)

3.1 WHEN `get_prices` is called with a `Vec<Symbol>` whose length is between 1 and `MAX_PATH_HOPS` (5) inclusive THEN the system SHALL CONTINUE TO return a `Vec<Option<PriceEntry>>` in the same order as the input symbols, with stale or missing assets represented as `None` entries.

3.2 WHEN `get_prices_with_status` is called with a `Vec<Symbol>` whose length is between 1 and `MAX_PATH_HOPS` (5) inclusive THEN the system SHALL CONTINUE TO return a `Vec<Option<PriceEntryWithStatus>>` with correct freshness flags for each asset.

3.3 WHEN `get_index_price` is called with a `Vec<AssetWeight>` whose length is between 1 and `MAX_PATH_HOPS` (5) inclusive THEN the system SHALL CONTINUE TO return the correct weighted-average index price.

3.4 WHEN `add_recipient` is called and the total recipient count after addition would be `MAX_RECIPIENTS` (50) or fewer THEN the system SHALL CONTINUE TO add the recipient, update the total-shares counter, and persist the updated list to storage.

3.5 WHEN `distribute` is called with a valid amount and a recipient list of up to `MAX_RECIPIENTS` (50) entries THEN the system SHALL CONTINUE TO transfer the proportional share amount to each recipient according to their configured basis-point weight.

3.6 WHEN `get_prices` is called with an empty `Vec<Symbol>` THEN the system SHALL CONTINUE TO return an empty result vector without error.

---

## Bug Condition Pseudocode

**Bug Condition — price-oracle batch reads and index price:**

```pascal
FUNCTION isBugCondition_Oracle(X)
  INPUT: X of type Vec<Symbol> or Vec<AssetWeight>
  OUTPUT: boolean

  RETURN X.len() > MAX_PATH_HOPS   // MAX_PATH_HOPS = 5
END FUNCTION
```

**Property: Fix Checking — Oracle**

```pascal
FOR ALL X WHERE isBugCondition_Oracle(X) DO
  result ← get_prices'(X)          // or get_prices_with_status'(X) / get_index_price'(X)
  ASSERT result = Err(ContractError::OutOfBounds)
  ASSERT no_budget_panic(result)
END FOR
```

**Bug Condition — reward-splitter recipient list:**

```pascal
FUNCTION isBugCondition_Splitter(stored_recipients)
  INPUT: stored_recipients of type Vec<Recipient>
  OUTPUT: boolean

  RETURN stored_recipients.len() >= MAX_RECIPIENTS   // MAX_RECIPIENTS = 50
END FUNCTION
```

**Property: Fix Checking — Splitter**

```pascal
FOR ALL state WHERE isBugCondition_Splitter(state.recipients) DO
  result ← add_recipient'(admin, new_address, share)
  ASSERT result = Err(Error::OutOfBounds)
END FOR
```

**Preservation Goal:**

```pascal
// Property: Preservation Checking
FOR ALL X WHERE NOT isBugCondition(X) DO
  ASSERT F(X) = F'(X)
END FOR
```
