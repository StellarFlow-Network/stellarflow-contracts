# Telemetry Validation Flow Diagram

## High-Level Architecture

```mermaid
flowchart TB
    A[Validator Submits Telemetry] --> B{Node Revoked?}
    B -->|Yes| Z1[❌ Reject: RevokedAddress]
    B -->|No| C{Authenticated?}
    C -->|No| Z2[❌ Reject: Unauthorized]
    C -->|Yes| D[Enter Validation Pipeline]
    
    D --> E{Timestamp Fresh?<br/>≤ 60 seconds}
    E -->|No| Z3[❌ Reject: StaleTelemetryPayload]
    E -->|Yes| F{Reserve A ≥ 100k XLM?}
    
    F -->|No| Z4[❌ Reject: InsufficientReserveBalance]
    F -->|Yes| G{Reserve B ≥ 100k XLM?}
    
    G -->|No| Z5[❌ Reject: InsufficientReserveBalance]
    G -->|Yes| H{Volume ≥ 10k XLM?}
    
    H -->|No| Z6[❌ Reject: InsufficientVolume]
    H -->|Yes| I{Validator Stake ≥ 1000?}
    
    I -->|No| Z7[❌ Reject: PremiumPoolAccessDenied]
    I -->|Yes| J[✅ All Checks Passed]
    
    J --> K[Record Heartbeat]
    K --> L[Emit telem_ok Event]
    L --> M[✅ Telemetry Accepted]
    
    style A fill:#e1f5ff
    style M fill:#d4edda
    style Z1 fill:#f8d7da
    style Z2 fill:#f8d7da
    style Z3 fill:#f8d7da
    style Z4 fill:#f8d7da
    style Z5 fill:#f8d7da
    style Z6 fill:#f8d7da
    style Z7 fill:#f8d7da
    style J fill:#d1ecf1
```

## Validation Pipeline Details

```mermaid
flowchart LR
    subgraph "Fast Checks (No Storage)"
        A1[Timestamp Check] -->|Pass| A2[Reserve A Check]
        A2 -->|Pass| A3[Reserve B Check]
        A3 -->|Pass| A4[Volume Check]
    end
    
    subgraph "Expensive Check (Storage Read)"
        B1[Bond Capacity Check]
    end
    
    A4 -->|Pass| B1
    B1 -->|Pass| C[Accept]
    
    A1 -->|Fail| D1[❌ Stale]
    A2 -->|Fail| D2[❌ Low Reserve]
    A3 -->|Fail| D3[❌ Low Reserve]
    A4 -->|Fail| D4[❌ Low Volume]
    B1 -->|Fail| D5[❌ No Bond]
    
    style C fill:#d4edda
    style D1 fill:#f8d7da
    style D2 fill:#f8d7da
    style D3 fill:#f8d7da
    style D4 fill:#f8d7da
    style D5 fill:#f8d7da
```

## Flash Loan Attack Prevention

```mermaid
sequenceDiagram
    participant Attacker
    participant FlashLoan
    participant ThinPool
    participant Validator
    participant StellarFlow
    
    Note over Attacker,StellarFlow: ❌ Attack Scenario (Pre-Implementation)
    
    Attacker->>FlashLoan: 1. Borrow 1M XLM
    FlashLoan->>Attacker: OK
    Attacker->>ThinPool: 2. Manipulate price (3k XLM pool)
    Note over ThinPool: Price distorted!
    Attacker->>Validator: 3. Request price submission
    Validator->>StellarFlow: 4. Submit manipulated price
    StellarFlow->>Validator: ✅ Accepted (no validation)
    Note over Attacker: 5. Profit from manipulation
    Attacker->>FlashLoan: 6. Repay loan
    
    Note over Attacker,StellarFlow: ✅ Protected Scenario (Post-Implementation)
    
    Attacker->>FlashLoan: 1. Borrow 1M XLM
    FlashLoan->>Attacker: OK
    Attacker->>ThinPool: 2. Attempt manipulation (3k XLM pool)
    Attacker->>Validator: 3. Request price submission
    Validator->>StellarFlow: 4. Submit telemetry<br/>(3k XLM reserves)
    StellarFlow->>StellarFlow: Validate: 3k < 100k
    StellarFlow->>Validator: ❌ REJECTED<br/>(InsufficientReserveBalance)
    Note over Attacker: Attack Thwarted!
    Attacker->>FlashLoan: Repay loan (no profit)
```

## Security Threshold Matrix

```mermaid
graph TD
    subgraph "Security Levels"
        A[Telemetry Submission] --> B{Reserve Balance Check}
        B -->|≥ 100k XLM| C[High Liquidity Pool ✅]
        B -->|10k-100k XLM| D[Medium Pool ⚠️]
        B -->|< 10k XLM| E[Thin Pool ❌]
        
        C --> F{Volume Check}
        F -->|≥ 10k XLM/24h| G[Active Market ✅]
        F -->|1k-10k XLM/24h| H[Low Activity ⚠️]
        F -->|< 1k XLM/24h| I[Dormant ❌]
        
        G --> J[✅ ACCEPTED]
        D --> K[❌ REJECTED]
        E --> K
        H --> K
        I --> K
    end
    
    style C fill:#d4edda
    style G fill:#d4edda
    style J fill:#d4edda
    style D fill:#fff3cd
    style H fill:#fff3cd
    style E fill:#f8d7da
    style I fill:#f8d7da
    style K fill:#f8d7da
```

## Data Flow

```mermaid
flowchart TB
    subgraph "On-Chain Data Sources"
        P1[Liquidity Pool]
        P2[Trading History]
        P3[Ledger Timestamp]
    end
    
    subgraph "Validator Node"
        V1[Query Pool State]
        V2[Calculate Metrics]
        V3[Submit Telemetry]
    end
    
    subgraph "StellarFlow Contract"
        S1[validate_telemetry_submission]
        S2[verify_payload_freshness]
        S3[validate_reserve_balance]
        S4[validate_trading_volume]
        S5[check_bond_capacity]
    end
    
    subgraph "Result"
        R1[✅ Accept & Record]
        R2[❌ Reject with Error]
    end
    
    P1 -->|Reserve Balances| V1
    P2 -->|24h Volume| V1
    P3 -->|Current Time| V1
    
    V1 --> V2
    V2 --> V3
    
    V3 --> S1
    S1 --> S2
    S2 --> S3
    S3 --> S4
    S4 --> S5
    
    S5 -->|All Pass| R1
    S2 -->|Fail| R2
    S3 -->|Fail| R2
    S4 -->|Fail| R2
    S5 -->|Fail| R2
    
    style R1 fill:#d4edda
    style R2 fill:#f8d7da
```

## Comparison: Before vs After

```mermaid
graph LR
    subgraph "Before Implementation"
        B1[Validator Submission] --> B2{Bond Check Only}
        B2 -->|Has Stake| B3[✅ Accept Any Pool]
        B2 -->|No Stake| B4[❌ Reject]
    end
    
    subgraph "After Implementation"
        A1[Validator Submission] --> A2{Timestamp?}
        A2 -->|Fresh| A3{Reserves?}
        A2 -->|Stale| A8[❌ Reject]
        A3 -->|High| A4{Volume?}
        A3 -->|Low| A9[❌ Reject]
        A4 -->|Active| A5{Bond?}
        A4 -->|Low| A10[❌ Reject]
        A5 -->|OK| A6[✅ Accept]
        A5 -->|No| A11[❌ Reject]
    end
    
    style B3 fill:#fff3cd
    style B4 fill:#f8d7da
    style A6 fill:#d4edda
    style A8 fill:#f8d7da
    style A9 fill:#f8d7da
    style A10 fill:#f8d7da
    style A11 fill:#f8d7da
```

## Error Distribution (Expected)

```mermaid
pie title Rejection Reasons (Estimated Distribution)
    "InsufficientReserveBalance" : 45
    "InsufficientVolume" : 30
    "StaleTelemetryPayload" : 15
    "PremiumPoolAccessDenied" : 10
```

## Component Interaction

```mermaid
graph TB
    subgraph "External"
        E1[Validator Node]
        E2[Liquidity Pool]
    end
    
    subgraph "StellarFlow Contract"
        C1[submit_telemetry_data]
        C2[validate_telemetry_submission]
        C3[validate_reserve_balance]
        C4[validate_trading_volume]
        C5[verify_payload_freshness]
        C6[check_bond_capacity]
    end
    
    subgraph "Storage"
        S1[Stake Registry]
        S2[Event Log]
    end
    
    E1 -->|Telemetry Data| C1
    E2 -.->|Reserve Data| E1
    
    C1 --> C2
    C2 --> C5
    C2 --> C3
    C2 --> C4
    C2 --> C6
    
    C6 -->|Read| S1
    C1 -->|Write| S2
    
    style E1 fill:#e1f5ff
    style C1 fill:#d1ecf1
    style C2 fill:#d1ecf1
```

## Testing Coverage

```mermaid
mindmap
  root((Validation Tests))
    Timestamp Freshness
      Within 60s ✅
      Exactly 60s ✅
      Beyond 60s ❌
      From future ✅
      Current time ✅
      Very stale ❌
    Reserve Balance
      Both above ✅
      Well above ✅
      First below ❌
      Second below ❌
      Both below ❌
      Negative ❌
      Zero ❌
    Trading Volume
      At threshold ✅
      Above threshold ✅
      Below threshold ❌
      Zero ❌
      Negative ❌
      High activity ✅
    Integration
      All pass ✅
      Stale first ❌
      Low reserves ❌
      Low volume ❌
      Attack scenario ❌
```

---

**Legend:**
- ✅ = Test passes / Telemetry accepted
- ❌ = Test fails / Telemetry rejected
- ⚠️ = Warning / Edge case
- 🔒 = Security check
- 💾 = Storage operation
- 📊 = Monitoring/Events
