Runs when none of the conditions of the `If` were true.

# Examples

```papyrus
# Actor akActor = Game.GetPlayer()
If akActor.IsDead()
    Debug.Trace("dead")
Else
    akActor.Kill()
EndIf
```
