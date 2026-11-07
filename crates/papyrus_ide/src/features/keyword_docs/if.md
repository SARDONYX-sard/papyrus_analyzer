Runs a block when a condition is true, optionally followed by `ElseIf` and `Else` blocks, and ended
by `EndIf`.

# Examples

```papyrus
# Int health = 5
If health < 10
    Debug.Notification("Low health")
ElseIf health < 50
    Debug.Notification("Hurt")
Else
    Debug.Notification("Fine")
EndIf
```
