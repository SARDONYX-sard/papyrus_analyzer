Returns the name of the current state of the script; `""` for the empty state.

# Examples

```papyrus
Function Toggle()
    If GetState() == "Open"
        GoToState("")
    Else
        GoToState("Open")
    EndIf
EndFunction

State Open
EndState
```
