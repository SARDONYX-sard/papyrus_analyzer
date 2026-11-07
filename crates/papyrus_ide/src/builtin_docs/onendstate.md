Sent when the script leaves this state, before `OnBeginState` of the next one.

# Examples

```papyrus
# ScriptName MyQuest extends Quest
State Waiting
    Event OnEndState()
        UnregisterForUpdate()
    EndEvent
EndState
```
