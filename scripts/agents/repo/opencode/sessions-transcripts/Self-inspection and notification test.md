# Self-inspection and notification test

## user

inspect yourself, then test if you can receive notification.

## assistant

**tool: `mvm-agent_inspect`**

input: `{}`

output:

```json
    {
      "budget": null,
      "capabilities": null,
      "children": [],
      "id": "6a87f7401214",
      "name": "opencode-agent",
      "parent": null,
      "ram_mib": 2048,
      "state": "running",
      "vcpus": 2
    }
```

**tool: `mvm-agent_test_notification`**

input: `{}`

output:

```json
    {
      "notifications": [
        {
          "error": null,
          "exit_code": 0,
          "kind": "child-ttl-about-to-expire",
          "ok": true,
          "output": ""
        },
        {
          "error": null,
          "exit_code": 0,
          "kind": "restarted-after-idle",
          "ok": true,
          "output": ""
        },
        {
          "error": null,
          "exit_code": 0,
          "kind": "need-input",
          "ok": true,
          "output": ""
        },
        {
          "error": null,
          "exit_code": 0,
          "kind": "finished",
          "ok": true,
          "output": ""
        },
        {
          "error": null,
          "exit_code": 0,
          "kind": "terminated",
          "ok": true,
          "output": ""
        },
        {
          "error": null,
          "exit_code": 0,
          "kind": "input",
          "ok": true,
          "output": ""
        }
      ]
    }
```

## user

```json
    {
      "id": "c4c80d3d4e894575bd9d6441cdd4df08",
      "created_at": "2026-08-18T14:39:53.046069Z",
      "from": {
        "from": "lifecycle-alert"
      },
      "type": {
        "type": "child-ttl-about-to-expire",
        "child": "test-child",
        "remaining_secs": 30
      }
    }
```

## user

```json
    {
      "id": "44d96060a4174a60b62c95f733443442",
      "created_at": "2026-08-18T14:39:53.046071Z",
      "from": {
        "from": "lifecycle-alert"
      },
      "type": {
        "type": "restarted-after-idle"
      }
    }
```

## user

```json
    {
      "id": "533c731b17a24131b5f028f6e5034ed1",
      "created_at": "2026-08-18T14:39:53.046073Z",
      "from": {
        "from": "child",
        "id": "test-child"
      },
      "type": {
        "type": "need-input",
        "data": {
          "prompt": "mock need-input"
        }
      }
    }
```

## user

```json
    {
      "id": "e89843572f974c1d976eb923c5bca804",
      "created_at": "2026-08-18T14:39:53.046075Z",
      "from": {
        "from": "child",
        "id": "test-child"
      },
      "type": {
        "type": "finished",
        "exit_code": 0,
        "data": {
          "result": "mock finished"
        }
      }
    }
```

## user

```json
    {
      "id": "d5f9f10712dc48c2bf1f271a75cb43f1",
      "created_at": "2026-08-18T14:39:53.046076Z",
      "from": {
        "from": "child",
        "id": "test-child"
      },
      "type": {
        "type": "terminated",
        "reason": "ttl-expired"
      }
    }
```

## user

```json
    {
      "id": "8654ddebc8394597847256831d26bebd",
      "created_at": "2026-08-18T14:39:53.046078Z",
      "from": {
        "from": "daddy"
      },
      "type": {
        "type": "input",
        "data": {
          "text": "mock input"
        }
      }
    }
```

## assistant

**Inspection Results:**
- ID: `6a87f7401214`
- Name: opencode-agent
- State: running
- Parent: none (top-level)
- Children: none
- RAM: 2048 MiB
- vCPUs: 2

**Notification Test Results:**
All 6 notification types received successfully:

| Kind | Status |
|------|--------|
| child-ttl-about-to-expire | ✓ |
| restarted-after-idle | ✓ |
| need-input | ✓ |
| finished | ✓ |
| terminated | ✓ |
| input | ✓ |

Notification system is fully operational.

## user

good , now shutdown

## assistant

**tool: `mvm-agent_stop`**

input: `{}`

status: running

