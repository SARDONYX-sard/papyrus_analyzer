Sent when the script leaves this state, before `OnBeginState` of the next one.

# Examples

```papyrus
State Waiting
    Event OnEndState()
        UnregisterForUpdate()
    EndEvent
EndState
```
