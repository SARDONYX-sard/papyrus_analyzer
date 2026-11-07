Converts a value to another type: casts between objects (`None` when the cast fails), and
conversions between `Int`, `Float`, `Bool` and `String`.

# Examples

```papyrus
# ObjectReference akRef = Game.GetPlayer()
Actor target = akRef as Actor
If target
    target.Kill()
EndIf
String text = 42 as String
Int whole = 3.7 as Int ; 3
```
