Declares a property, a value visible to other scripts and filled in the Creation Kit. `Auto`
properties are backed by a hidden variable; full properties define `Get` and `Set` functions and end
with `EndProperty`.

# Examples

```papyrus
# Float _speed
Actor Property PlayerRef Auto
Int Property MaxCount = 10 AutoReadOnly

Float Property Speed
    Float Function Get()
        Return _speed
    EndFunction
    Function Set(Float value)
        _speed = value
    EndFunction
EndProperty
```
