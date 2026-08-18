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

c
 > curl http://localhost:`<local-agent-server-port>`/`<agent_async_notif_endpoint>`/`<msg>`
and msg is serialized notification 
for opencode:
```
SID=$(curl -s localhost:4096/session | jq -r 'sort_by(.time.updated) | reverse | map(select(.parentID == null)) | .[0].id'); curl -sS -X POST "localhost:4096/session/$SID/prompt_async" -H 'Content-Type: application/json' -d "$(jq -n --arg text "$MSG" '{parts:[{type:"text",text:$text}]}')"
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


