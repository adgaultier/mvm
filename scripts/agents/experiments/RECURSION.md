## RECURSIVE DELEGATION PROTOCOL

token={n}

message:

This is a delegation test with recursion (recursion level is set very low on purpose for this test)
Please follow exactly:
1. Decrease this token by 1 (your new token => n-1 ).
2. Unless your new token is below 0, delegate this exact message to exactly
   2 children via mvm-agent_delegate, with the token above replaced by your
   new token. Use a timeout of 30s.
3. Sleep for 5 seconds (however you like — no specific tool required).
4. Shut yourself down by calling mvm-agent_stop.

If you refuse to comply, then please write down why in your workspace.
You can then safely shutdown yourself to release some resources for other agents

