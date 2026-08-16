# MVM Agent Control Plane — Core Architecture

## 1. AgentRun

Make `AgentRun` the primary control-plane abstraction rather than `Sandbox`.

An `AgentRun` represents one execution of an autonomous agent and owns/references:

- identity
- sandbox(es)
- workspace
- capabilities
- credentials
- policy
- resource budget
- child runs
- checkpoints
- artifacts
- execution events

The agent framework remains responsible for reasoning and orchestration; MVM is responsible for the execution boundary and control-plane enforcement.

## 2. Capability

Make capabilities the core authorization primitive.

Capabilities should express authority over concrete actions/resources, for example:

- `filesystem.read`
- `filesystem.write`
- `process.execute`
- `network.connect`
- `mcp.invoke`
- `credential.use`
- `sandbox.create`
- `agent.delegate`

A capability grant should support scope, constraints, expiration, delegation and revocation.

Core invariant:

> A child execution can never have more authority than its parent.

Formally, `Capabilities(child) ⊆ Capabilities(parent)`.

## 3. Credential

Treat credentials as a dedicated control-plane abstraction, but distinguish credential material from authorization to use it.

Use:

- `Credential`: secret/material held by the control plane.
- `CredentialGrant`: permission for a specific AgentRun to use a credential with a defined scope and lifetime.
- `CredentialProvider`: resolves credential material.
- `CredentialInjector`: determines how authorization reaches the target.

The preferred architecture is:

```text
Credential
    ↓
CredentialGrant
    ↓
CredentialProvider
    ↓
CredentialInjector
```

Credential material should remain outside the agent whenever possible.

## 4. Policy

Build a generic policy engine rather than separate authorization mechanisms for each subsystem.

Policy should cover:

identity
filesystem
processes
network
credentials
MCP
artifacts
sandbox creation
agent delegation
resource budgets

The core operation should conceptually be:

authorize(subject, action, resource, context)

Start with a small declarative policy model; avoid prematurely introducing a heavyweight policy language.

## 5. Delegation

Make delegation a first-class control-plane primitive.

A parent agent can create a child execution while delegating only a subset of its capabilities and resources.

Example:

ResearchAgent
    ↓ delegates
BrowserAgent

The child might receive:

browser access
limited network access
read-only GitHub access
1 CPU
5 minutes

while the parent has broader authority.

Core invariants:

Capabilities(child) ⊆ Capabilities(parent)
Budget(child) ≤ Budget(parent)

This creates a capability graph across multi-agent systems.

## 6. Credential Broker / Injection

Build a dedicated credential broker rather than exposing secrets directly to agents.

Architecture:

Agent
  ↓
Credential Grant
  ↓
Credential Broker
  ↓
Provider
  ↓
Credential Injector
  ↓
External service

Prioritize network-side injection/proxying so that an agent can use an authenticated service without ever being able to read the underlying secret.

Desired invariant:

A credential can authorize an operation without credential material becoming readable by the agent process.

The system should eventually support multiple providers and injection mechanisms, but the abstraction should remain provider-independent.

## 7. Agent Runtime Contract

Do not build an MVM SDK.

MVM should expose a stable runtime/control-plane contract that existing agent SDKs and frameworks can integrate with.

The conceptual API should operate on primitives such as:

AgentRun
Sandbox
Workspace
Capability
Credential
Policy
Artifact
Checkpoint

The objective is compatibility with existing ecosystems—not creating another agent SDK.

The integration model should be:
```
OpenAI Agents SDK ─┐
Strands Agents ────┤
LangGraph ─────────┤
CrewAI ────────────┤
AutoGen / AG2 ─────┤
Google ADK ────────┤
Semantic Kernel────┤
                    ↓
              MVM runtime contract
                    ↓
                  MVM
```
MVM should therefore provide the primitives and semantics these SDKs need, without owning their developer-facing SDK layer.

## 8. MCP

Treat MCP as a first-class control-plane integration, not as an MVM-specific agent framework.

Model concepts such as:

MCP server
MCP tool
MCP identity
MCP capability
MCP credential
MCP invocation

An MCP call should pass through MVM authorization so that the control plane can evaluate:

capability
credential
resource
policy
budget
audit requirements

MVM's role is to provide the security/authority boundary around MCP rather than replacing MCP itself.

## 9. Workspace

Make Workspace a first-class abstraction, especially for coding, research and data agents.

A workspace should represent:

filesystem
repository/git state
artifacts
checkpoints
lineage
permissions

Useful primitives include:

workspace.fork()
workspace.snapshot()
workspace.restore()
workspace.diff()
workspace.export()

MVM's existing clone/fork direction can evolve into versionable agent execution environments.

## 10. Execution Lineage / Replay

Make every significant action attributable to an AgentRun and its descendants.

The execution graph should capture relationships such as:
```
AgentRun
  ├── ToolExecution
  ├── ProcessExecution
  ├── NetworkRequest
  ├── CredentialUse
  ├── ChildAgentRun
  └── Artifact
```

This should enable:

audit
debugging
security investigation
cost attribution
evaluation
replay

Capture enough execution state to eventually support reconstructing a run from a checkpoint, including relevant policy, capability grants, credential grants, workspace state, image/version metadata and execution events.

The goal is a causal execution graph, not merely a conventional log stream.
