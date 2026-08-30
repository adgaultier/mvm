//! Host side of the Agent API: VM-authenticated control between the guest's
//! `mvm-agent-mcp` bridge and the sandbox manager. All of this module only
//! exists under the `agent-api` feature — token *minting* into the guest env
//! stays in `lib.rs` regardless.

pub(crate) mod agent_api;
pub(crate) mod delegate;
pub(crate) mod notifications;

pub(crate) use agent_api::spawn_accept_loop;
