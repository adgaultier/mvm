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


 > curl http://localhost:`<local-agent-server-port>`/`<agent_async_notif_endpoint>`/`<msg>`
and msg is the notification rendered as human-readable text
(`Notification::to_text` in `crates/common/src/agent_api.rs`), e.g.
`Daddy is requesting: <task>`, `Child <id> is requesting input: <data>`,
`Child <id> finished (exit code <n>): <data>`,
`Child <id> was terminated (faulted | TTL expired)`,
`You were restarted after an idle stop; continue your work.`,
`Child <id> is about to hit its TTL (<n>s left)`
for opencode:
```
SID=$(curl -s localhost:4096/session | jq -r 'sort_by(.time.updated) | reverse | map(select(.parentID == null)) | .[0].id'); curl -sS -X POST "localhost:4096/session/$SID/prompt_async" -H 'Content-Type: application/json' -d "$(jq -n --arg text '<MSG>' '{parts:[{type:"text",text:$text}]}')"
```
```
                    Message
                       │
            ┌──────────┴──────────┐
            │                     │
        lifecycle               task
            │                     │
      ┌─────┴──────┐        ┌─────┴─────┐
      │            │        │           │
    ttl          restart   input      result
    terminated             request     completed
┌─────────────────────────────┐
│ Agent Data Plane            │
│                             │
│ A2A                         │
│   ├── task                  │
│   ├── task update           │
│   ├── input-required        │
│   └── artifact/result       │
│                             │
│ Platform Events             │
│   ├── ttl-warning           │
│   ├── restarted             │
│   └── terminated            │
└─────────────────────────────┘
```
## IDLE AGENTS
if an agent is idle, then :
- incoming notifications are put into a queue
- agent is restarted `mvm start`
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



## RUNTIME 
### vercel FX
➜  ` time fx ask "Reply with exactly: FX  TEST OK"`
```
𝒇x v0.0.3 · Run /help for commands

┃ Reply with exactly: FX  TEST OK

  FX  TEST OK
fx ask "Reply with exactly: FX  TEST OK"  
```
>> 0,04s user 0,06s system 1% cpu 5,886 total

### Opencode
➜  `time opencode run  "Reply with exactly: opencode TEST OK"`
```
> build · deepseek-v4-flash-free

opencode TEST OK

"Reply with exactly: opencode TEST OK"

```
>>  2,61s user 0,69s system 65% cpu 5,019 total