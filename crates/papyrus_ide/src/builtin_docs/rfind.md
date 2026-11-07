Returns the index of the last element equal to `akElement`, searching backwards from `aiStartIndex`
(-1: the last element), or -1.

# Examples

```papyrus
Int[] rolls = New Int[3]
rolls[0] = 6
rolls[1] = 2
rolls[2] = 6
Int last = rolls.RFind(6)        ; 2
Int before = rolls.RFind(6, 1)   ; 0
```
