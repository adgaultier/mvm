#  NOTIFICATIONS / TIMEOUTS / DELEGATION

## NOTIFICATION SPECS
```
notifications:
    from: 
        - daddy
        - lifecycle_alert
        - child(id)
    type:
        lifcecyle_alert:
            - child-ttl-about-to-expire-notif
            - restarted-after-idle-notif
        child(id):
            - need-input-notif
            - finished-notif
            - terminated (faulted / ttl )-notif
        daddy:
            - input-notif
```
all notification are passed asyncronously to running agents via via `mvm exec <async_cmd>`

where `async_cmd` is typically :
 > curl http://localhost:`<local-agent-server-port>`/`<agent_async_notif_endpoint>`/`<msg>`
and msg is serialized notification 


#### GVPROXY LIMITATIONS FOR GUEST → AGENT API COMMUNICATION
Notifications from the guest must never reach the control-plane API, which is served exclusively on the host loopback interface (127.0.0.1).
With the current gvproxy setup, the guest can reach a host service only if both of the following conditions are met:
- The the agent-api is served  on 0.0.0.0 (i.e. is exposed beyond loopback), which is possibile with `mwm serve  --agent-addr  0.0.0.0:24643`
- The gvproxy virtual gateway address (currently hardcoded to 192.168.127.1) corresponds to a real host LAN IP. Which we don't want 


Conclusion: gvproxy does not provide the guest → host-loopback forwarding we need, so it is not suitable for this communication path.

Solution 1) : Use passt as the network backend, as it allows the guest to reach services bound to the host loopback interface (127.0.0.1).

-> SOLUTION dont use HTTP TRANSPORT at all it should be like
```
daemon
  │
  ├── vsock :5000 ──► guest control agent
  │                    - exec
  │                    - PTY
  │                    - stdin/stdout
  │                    - lifecycle 
  │
  └── vsock :5001 ◄── guest Agent API
                       - workload → host requests via mcp
```
## IDLE AGENTS
if an agent is idle, then :
- incoming notifications are put into a queue
- agent is restarted `mvm start`
- notif mesage should basically say to continue to work, after having consumed notification queue
- vm start command  + mounts are responsible to restart from saved state:
    - with overlayfs storage backend:`opencode -c `
    - with copy storage backend: `opencode -c ` + `OPENCODE_DB=/home/agent/opencode/sessions.db` where `/home/agent/opencode`  is a  bind mount  


## TIMEOUTS
> timeouts are tracked and managed by agent control plane
### IDLE
- idle timeout is opt-in , default is none
- this mechanism only exist to give back compute/memory ressources to the ressource pool when agent isnt working
- agent is SIGTERM by control plane `mvm stop`

## TTL
- ttl is opt-in , default is none
- hard constraint
- agent AND children are (SIGKILLED BY  CONTROL PLANE) `mvm stop && mvm rm`


