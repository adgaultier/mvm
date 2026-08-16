# MVM × Agent Ecosystem — MicroVM Compatibility & Synergies

## 1. Agent frameworks that can run on MVM

| Project / ecosystem | MVM compatibility | Synergy |
|---|---|---|
| **OpenAI Agents SDK** | 🟢 Excellent | Explicit sandbox/runtime concepts make it a strong target for MVM as the sandbox provider. |
| **Strands Agents** | 🟢 Excellent | Framework/runtime separation makes it a natural MVM execution target; AWS credential/identity integration is particularly interesting. |
| **LangGraph** | 🟢 Excellent | Orchestration can run above MVM while MVM provides isolated execution, state and resources. |
| **CrewAI** | 🟢 Excellent | Agents/tasks can execute inside MVM without MVM needing to understand CrewAI's orchestration semantics. |
| **AutoGen / AG2** | 🟢 Excellent | Multi-agent execution maps naturally onto MVM's AgentRun, capability and delegation model. |
| **Google ADK** | 🟢 Excellent | Agent orchestration can sit above MVM; MCP/A2A and credential boundaries are especially relevant. |
| **Microsoft Agent Framework / Semantic Kernel** | 🟢 Excellent | Framework orchestrates; MVM controls execution, isolation, capabilities and credentials. |
| **PydanticAI** | 🟢 Excellent | Lightweight runtime makes it straightforward to execute agent processes inside MVM. |
| **Smolagents** | 🟢 Excellent | Generated code execution is a strong use case for microVM isolation. |
| **Claude Code / coding agents** | 🟢 Excellent | Strong fit for isolated filesystem/process/network execution and credential injection. |
| **Aider** | 🟢 Excellent | Git workspace + shell + network + credentials map directly to MVM primitives. |
| **Open Interpreter** | 🟢 Excellent | Code execution is an obvious microVM isolation use case. |
| **AutoGPT** | 🟢 Excellent | Command, filesystem and network execution can sit behind MVM's execution boundary. |
| **DeerFlow** | 🟢 Excellent | Long-running workflows and sub-agents map well to AgentRun, delegation and workspace abstractions. |
| **Omnigent** | 🟢 Very interesting | Already abstracts over multiple sandbox providers; MVM could become another execution backend. |

## 2. MicroVM / agent-runtime projects to study as architectural neighbors

| Project | Relevance to MVM |
|---|---|
| **E2B** | Direct architectural comparison; Firecracker-based agent sandboxes. More competitor/reference than integration target. |
| **forkd** | Firecracker/KVM with copy-on-write VM forking; highly relevant to MVM snapshot/fork semantics. |
| **SmolVM** | Firecracker microVM runtime; relevant to lightweight agent execution. |
| **Cleanroom** | Firecracker + credential gateway; directly relevant to MVM credential injection. |
| **matchlock** | Firecracker + host-side secret injection; directly relevant to secret isolation. |
| **microsandbox** | libkrun/KVM with secret isolation; relevant to MVM's security model. |
| **OpenSandbox** | Multiple sandbox backends including Firecracker/Kata/gVisor/Docker; relevant as a common execution abstraction. |
| **Gondolin** | QEMU microVM approach; useful architectural comparison. |
| **K7** | Kata/microVM-oriented isolation; relevant to runtime abstraction. |
| **Stockyard** | Firecracker + snapshotting; relevant to fast agent startup and workspace lifecycle. |
| **Beams** | Firecracker + delegated identity + zero-secret patterns; highly relevant to MVM credential/delegation architecture. |
| **navaris** | Firecracker/LXC sandbox control plane; relevant to MVM's control-plane direction. |
| **Docker Sandboxes** | MicroVM-backed agent isolation; useful reference for developer experience and compatibility. |

## 3. Strategic compatibility model

MVM should **not** require every agent framework to implement a special "microVM API".

Instead:

```text
                 Agent framework
                       │
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
    Strands        LangGraph       CrewAI
        │              │              │
        └──────────────┼──────────────┘
                       ▼
                Agent execution
                       │
                       ▼
                 ┌──────────┐
                 │   MVM    │
                 │ AgentRun │
                 └────┬─────┘
                      │
                  ┌───▼───┐
                  │microVM│
                  └───────┘
