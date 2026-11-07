Declares an event handler, called by the game (or by `SendModEvent`). Events never return a value.
The body ends with `EndEvent`.

# Examples

```papyrus
# ScriptName MyQuest extends Quest
Event OnInit()
    RegisterForSingleUpdate(5.0)
EndEvent

Event OnUpdate()
    Debug.Trace("tick")
EndEvent
```
