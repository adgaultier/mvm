# MVM — Agent-native execution architecture

## 1. Core idea

MVM is a secure execution substrate for autonomous agents.

It provides:

* hardware-isolated microVMs
* OCI-based execution
* sandbox-scoped identity
* capability-based delegation
* recursive execution
* resource budgets
* lifecycle and revocation

The **agent orchestrator owns the workflow**.

MVM owns the **execution boundary**.

---

## 2. Architecture

```text
        Agent orchestrator
        ┌──────┬──────┬──────┐
        │      │      │      │
     Strands  ...   ...   custom
        │      │      │      │
        └──────┴──────┴──────┘
                   │
                   ▼
             MVM Agent API
                   │
                   ▼
          Root sandbox / capability
                   │
                   ▼
          recursive delegation
                   │
                   ▼
              microVM tree
```

MVM does not need to know which agent framework is orchestrating the workflow.

The orchestrator decides **what should happen**; MVM determines **where it executes and under which constraints**.

---

## 3. Control API vs Agent API

MVM has two distinct API planes.

### Control API

Used by authenticated humans/operators to manage the MVM host.

```text
Human / Operator
       │
       │ authenticated
       ▼
┌─────────────┐
│ Control API │
└──────┬──────┘
       │
       ▼
    MVM Host
```

Typical responsibilities include host-level and sandbox management.

### Agent API

Used by autonomous workloads through sandbox-scoped capabilities.

```text
Agent / Orchestrator
        │
        ▼
  MVM Agent API
        │
        ▼
   Root sandbox
```

The agent plane is independent from the human control plane.

---

## 4. Sandbox identity and capability

Each sandbox receives its own sandbox-scoped token.

```text
┌───────────────────────┐
│       Sandbox         │
│                       │
│  sandbox-scoped token │
└───────────┬───────────┘
            │
            │ Bearer token
            ▼
      ┌──────────────┐
      │  Agent API   │
      └──────────────┘
```

The Agent API derives the sandbox identity from the token rather than trusting a sandbox ID supplied by the caller.

The token therefore establishes:

* **who** is calling
* **which sandbox** it represents
* **which sandbox-scoped operations** it can perform

Tokens are tied to the sandbox lifecycle.

---

## 5. Root sandbox

An agent workflow can start with a minimal root sandbox.

```text
        Agent orchestrator
                │
                ▼
        ┌───────────────┐
        │ Root sandbox  │
        │               │
        │ identity      │
        │ capability    │
        └───────┬───────┘
                │
             delegate
                │
                ▼
        delegated sandbox
```

The root sandbox provides the initial capability from which further execution can be delegated.

It does not need to perform every task itself.

---

## 6. Recursive delegation

A sandbox can delegate work to additional sandboxes.

```text
                    Root
                     │
          ┌──────────┼──────────┐
          ▼          ▼          ▼
          A          B          C
          │
      ┌───┴────┐
      ▼        ▼
     A1       A2
     │
   ┌─┴─┐
   ▼   ▼
  A1a A1b
```

Every delegated sandbox has its own:

* identity
* resource limits
* lifecycle
* permissions

A delegated sandbox can itself delegate further.

### Core invariant

> **A child cannot obtain more authority or resources than its parent possesses.**

This creates a recursive capability tree.

---

## 7. Capability delegation

Delegation should produce a bounded child capability.

```text
Parent capability
       │
       ▼
   delegation
       │
       ├── permissions
       ├── resource limits
       ├── workflow
       ├── lifetime
       └── revocation
       │
       ▼
Child capability
```

The child receives only the authority explicitly delegated by its parent.

Capabilities should be enforceable independently of the guest's cooperation.

---

## 8. Resource budgets

Resource governance exists at multiple levels.

```text
                     HOST
                64 CPU / 128 GB
                       │
                       ▼
                   WORKFLOW
                16 CPU / 32 GB
                       │
                       ▼
                     AGENT
                 8 CPU / 16 GB
                       │
                       ▼
                     TASK
                  4 CPU / 8 GB
```

A child cannot exceed the budget available to its ancestors.

Conceptually:

```text
effective allocation =
    min(
        host capacity,
        workflow budget,
        parent remaining budget,
        requested allocation
    )
```

Budgets can cover:

* CPU
* memory
* concurrent sandboxes
* execution duration
* storage
* network / egress
* process count
* other host resources

The host enforces the hard limits. The guest must not be trusted to enforce them.

---

## 9. Workflow as a resource tree

The delegation tree is also a resource tree.

```text
Workflow
│
├── Agent A              8 CPU / 16 GB
│   │
│   ├── Task A1          4 CPU / 8 GB
│   │   ├── Child A1a   1 CPU / 2 GB
│   │   └── Child A1b   1 CPU / 2 GB
│   │
│   └── Task A2          2 CPU / 4 GB
│
└── Agent B              4 CPU / 8 GB
```

This allows autonomous workloads to parallelize while remaining bounded by the workflow's total budget.

The global host budget remains the ultimate constraint.

---

## 10. Lifecycle and revocation

The lifecycle of a delegated execution can be represented as:

```text
create
  │
  ▼
root sandbox
  │
  ▼
delegate
  │
  ▼
child sandbox
  │
  ├──── execute
  │
  ├──── delegate again
  │          │
  │          ▼
  │      child sandbox
  │
  ▼
revoke
  │
  ▼
cleanup
```

Revocation should propagate through the delegation tree.

```text
        Agent A
           │
      ┌────┴────┐
      ▼         ▼
     A1         A2
    ┌─┴─┐
    ▼   ▼
   A1a A1b

Revoke A
   │
   └──► A1, A2, A1a, A1b
        become invalid
```

This makes the delegation tree both a **security hierarchy** and a **lifecycle hierarchy**.

---

## 11. MVM's responsibility

```text
Agent orchestrator
        │
        │ workflow / decisions
        ▼
   MVM Agent API
        │
        │ capabilities / delegation
        ▼
    microVM tree
        │
        ├── isolation
        ├── resource enforcement
        ├── identity
        ├── lifecycle
        └── revocation
```

**The orchestrator owns the workflow.**

**MVM owns the execution boundary.**
