Sent when the script enters this state, after `OnEndState` of the previous one.

# Examples

```papyrus
# ScriptName MyQuest extends Quest
State Waiting
    Event OnBeginState()
        RegisterForSingleUpdate(10.0)
    EndEvent
EndState
```
