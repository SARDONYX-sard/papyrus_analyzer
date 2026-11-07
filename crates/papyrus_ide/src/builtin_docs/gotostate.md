Switches the script to the state `asNewState`: `OnEndState` runs on the old state, then
`OnBeginState` on the new one. `""` is the empty state, outside of any `State`.

# Examples

```papyrus
Event OnActivate(ObjectReference akActionRef)
    GoToState("Busy")
    Utility.Wait(5.0)
    GoToState("")
EndEvent

State Busy
    Event OnActivate(ObjectReference akActionRef)
        ; ignored while busy
    EndEvent
EndState
```
