# E2B MicroVM Agents — Architecture in 10 Points

## 1. MicroVM = hard isolation boundary — **M5 / Core**

Each E2B Sandbox is a **Firecracker microVM** with its own Linux kernel, memory, filesystem and processes. This is the fundamental security boundary for untrusted agent code.

```text
Agent → E2B Sandbox → Firecracker VM → Linux
```

## 2. Snapshot + COW = fast, reproducible agents — **M5 / Core**

Sandboxes are created from prebuilt VM snapshots/templates, with **copy-on-write filesystems** and lazy memory restoration via UFFD.

```text
Template snapshot → COW Sandbox → Agent
```

This gives fast startup while preserving an isolated mutable workspace.

## 3. `envd` = execution gateway inside the VM — **M5 / Core**

`envd` runs inside each sandbox and exposes typed APIs for:

* processes;
* stdin/stdout/stderr;
* PTY;
* filesystem;
* lifecycle;
* streaming.

```text
E2B API → Orchestrator → VM → envd → Agent/processes
```

## 4. Control plane ≠ data plane — **M5 / Core**

E2B separates lifecycle/control traffic from sandbox application traffic.

```text
CONTROL: SDK → API → Orchestrator
DATA:    Client → Proxy → Sandbox
```

This prevents agent/application traffic from turning the central API into a bottleneck.

## 5. Persistence is VM-level — **M5 / Core**

Pause/resume snapshots preserve sandbox state:

```text
Agent → VM state → Snapshot → Resume → Agent
```

The important distinction is that E2B persists the **execution environment**, not inherently the LLM's conversation/context.

## 6. Agent delegation = multiple isolated Sandboxes — **M4 / Established**

The current OpenAI Agents SDK integration explicitly supports a coordinator spawning parallel sandbox workers:

```text
              Coordinator
          ┌──────┼──────┐
          ▼      ▼      ▼
        VM-A   VM-B   VM-C
       Worker Worker Worker
```

Each worker gets an independent filesystem, process tree and VM.

## 7. E2B does NOT provide a native agent message bus — **M0 / Not identified**

There is no core E2B primitive equivalent to:

```text
send_message(agent_A, agent_B)
```

Delegation semantics remain the responsibility of the **agent framework/application**.

Typical communication is:

```text
Worker → final result → Coordinator
```

## 8. Inter-agent state is explicit — **M4**

Agents can exchange information through application-level mechanisms:

* coordinator result passing;
* files/artifacts;
* Git branches/patches;
* HTTP services;
* external queues;
* shared databases;
* MCP.

This is intentionally different from implicit shared-memory agents.

## 9. MCP = standardized agent ↔ tool communication — **M4 / Established**

E2B increasingly uses MCP as the tool interoperability layer:

```text
Agent
  │
  ▼
MCP Gateway
  ├── Browser
  ├── Search
  ├── GitHub
  └── Custom tools
```

MCP servers can run inside the isolated sandbox, further containing untrusted tooling.

## 10. Overall architecture — **Core design**

```text
                 AGENT FRAMEWORK
          planning / delegation / synthesis
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
       E2B VM A     E2B VM B     E2B VM C
       Worker A     Worker B     Worker C
          │            │            │
         MCP          MCP          MCP
          │            │            │
          └────────────┼────────────┘
                       ▼
                  Coordinator
                       │
                       ▼
                  Final result
```

### Maturity summary

| Architecture                  |                   Maturity |
| ----------------------------- | -------------------------: |
| Firecracker isolation         |              **M5 — Core** |
| Snapshot/COW/UFFD             |              **M5 — Core** |
| `envd` execution layer        |              **M5 — Core** |
| Control/data-plane separation |              **M5 — Core** |
| Pause/resume persistence      |              **M5 — Core** |
| Parallel sandbox workers      |       **M4 — Established** |
| OpenAI Agents SDK integration | **M4 — Established/Newer** |
| MCP integration               |       **M4 — Established** |
| Native agent-to-agent bus     |    **M0 — Not identified** |
| eBPF checkpoint optimization  |      **M2 — Experimental** |

### One-line takeaway

> **E2B provides the isolated, persistent execution substrate for delegated agents; the agent framework provides delegation and inter-agent semantics, while MCP provides standardized tool communication.**


# E2B → mvm: Agent Architecture Adaptation


## Core Principle


**mvm is a general-purpose microVM runtime. Agents are an optional consumer of mvm, implemented through a separate Agent Control Plane.**


The two abstractions must remain independent.


```text
                         mvm
                          │
              ┌───────────┴───────────┐
              │                       │
       MicroVM Control Plane    Agent Control Plane
              │                       │
       generic VM semantics      agent semantics
              │                       │
       create / clone / fork     create / fork / delegate
              │                       │
              └───────────┬───────────┘
                          │
                       MicroVMs
```                   
## 1. Keep MicroVM clone/fork generic

The existing microVM API must remain fully usable without agents.

POST /api/v1/sandboxes/{id}/clone
POST /api/v1/sandboxes/{id}/fork

Semantics:

VM → VM

The MicroVM Control Plane knows nothing about:

agents
tasks
delegation
MCP
coordinators
agent identity

A user should be able to use mvm purely as a microVM runtime.

## 2. Add a separate Agent Control Plane

Agent functionality belongs behind a separate API.
```     
POST /agent/v1/agents
POST /agent/v1/agents/{id}/fork
POST /agent/v1/agents/{id}/delegate
POST /agent/v1/tasks/{id}/cancel
GET  /agent/v1/tasks/{id}
```     
Semantics:

Agent → Agent

The Agent Control Plane may internally use the MicroVM Control Plane:
```     
Agent fork
    │
    ▼
mvm clone/fork
    │
    ▼
new VM
    │
    ▼
initialize mvm-agent
    │
    ▼
new Agent
```     
The user-facing abstractions nevertheless remain separate.

## 3. Enforce a one-way dependency
```     
Agent Control Plane
        │
        │ uses
        ▼
MicroVM Control Plane
        │
        ▼
      libkrun
```     
Never:

MicroVM Runtime → Agent concepts

Therefore:

Every agent can run on mvm, but mvm does not know or care whether a workload is an agent.

## 4. Import E2B's coordinator/worker model

Adapt E2B's orchestration at the agent layer:
```     
Coordinator Agent
       │
       ├── Worker Agent A
       ├── Worker Agent B
       └── Worker Agent C
```     
Each worker gets its own microVM.

The MicroVM Runtime provides:

isolation
resources
filesystem
networking
process execution
lifecycle

The Agent Control Plane provides:

delegation
task semantics
worker lifecycle
result collection
coordination

## s5. Keep MicroVM fork and Agent fork semantically different
MicroVM fork
VM A ──────► VM B

Meaning:

Give me another VM based on this VM.

Agent fork
Agent A ───► Agent B

Meaning:

Create another agent execution context based on this agent and assign it work.

Agent fork may internally use VM fork, but the APIs and semantics remain separate.

## 6. Separate the lifecycles
```     
MicroVM lifecycle
MicroVM
 ├── create
 ├── start
 ├── stop
 ├── clone
 ├── fork
 ├── snapshot
 └── restore
Agent lifecycle
Agent
 ├── create
 ├── start
 ├── fork
 ├── delegate
 ├── cancel
 ├── wait
 └── result
```     
This allows non-agent mvm users to remain completely unaffected by agent functionality.

## 7. Add structured agent tasks/results

Import the higher-level E2B agent semantics without putting them into the VM API.
```     
Task
 ├── id
 ├── description
 ├── capabilities
 ├── resources
 └── timeout


Result
 ├── status
 ├── summary
 ├── artifacts
 └── error
```     
The MicroVM API should remain focused on VM/process-oriented primitives.

## 8. Keep mvm-agent as the guest-side interface

Do not replace mvm-agent with an E2B-style envd.

Keep:
```     
VM
└── mvm-agent
      │
     vsock
      │
      ▼
Agent Control Plane
```     
mvm-agent can evolve to support:

task execution
structured events
capabilities
process control
filesystem access
agent lifecycle support

while remaining independent from the generic MicroVM API.

## 9. Keep MCP and artifacts above the VM layer

MCP should be an agent capability:
```     
Agent
 ├── MCP
 ├── tools
 ├── delegation
 └── tasks
       │
       ▼
      mvm
```     
Artifacts should likewise belong to the agent layer:
```     
Agent A
   │
   ▼
Artifact
   │
   ▼
Agent B
```     
mvm provides the isolated execution environment; the Agent Control Plane defines the semantics of tasks and artifacts.

## 10. Do NOT import E2B's cloud infrastructure

Do not introduce the following into core mvm:

Cloud scheduler
Distributed node placement
PostgreSQL/Redis control plane
Object-storage snapshot distribution
Cloud proxy layer
Multi-tenancy
E2B envd
Native agent message bus

Also, E2B-style VM memory snapshots should be considered a future optimization, not a prerequisite for agents.
```     
Target Architecture
                         ┌─────────────────────┐
                         │        mvm           │
                         │  MicroVM Runtime     │
                         └──────────┬──────────┘
                                    │
             ┌──────────────────────┴─────────────────────┐
             │                                            │
             ▼                                            ▼
   ┌─────────────────────┐                    ┌─────────────────────┐
   │ MicroVM Control     │                    │ Agent Control       │
   │ Plane               │                    │ Plane               │
   │                     │                    │                     │
   │ create              │                    │ create_agent        │
   │ clone               │                    │ fork_agent          │
   │ fork                │                    │ delegate            │
   │ start/stop          │                    │ task/result         │
   │ snapshot/restore    │                    │ cancel/wait         │
   └──────────┬──────────┘                    └──────────┬──────────┘
              │                                          │
              ▼                                          ▼
           MicroVMs                                  mvm-agent
                                                         │
                                                       vsock
```                                                           

## Final Design Rule

MicroVM Control Plane = infrastructure semantics.

Agent Control Plane = agent semantics.

Agent Control Plane may consume the MicroVM Control Plane, but the reverse dependency must never exist.

This preserves mvm as a standalone microVM runtime while allowing E2B-inspired delegation, coordinator/worker execution, agent forking, structured tasks/results, and isolated multi-agent workloads to be built cleanly on top.

TODO
- Keep generic MicroVM clone/fork completely agent-agnostic.
- Define separate /agent/v1/... API.
- Define Agent, Task, Result, Artifact, and Capability models.
- Implement agent fork independently from VM fork.
- Implement agent delegate.
- Define how Agent Control Plane invokes the MicroVM Control Plane.
- Extend mvm-agent for agent/task lifecycle without coupling it to the generic VM API.
- Add structured task/result/event streaming over the existing vsock architecture.
- Keep MCP entirely at the agent layer.
- Keep agent artifacts separate from generic VM filesystem semantics.
- Postpone cloud scheduler, distributed orchestration, multi-tenancy, and E2B-style memory snapshots.
