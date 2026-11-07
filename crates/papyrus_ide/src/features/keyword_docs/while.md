Repeats a block as long as a condition is true, ended by `EndWhile`. Papyrus has no `for` loop or
`break`.

# Examples

```papyrus
# Int[] items = New Int[3]
Int i = 0
While i < items.Length
    Debug.Trace(items[i])
    i += 1
EndWhile
```
