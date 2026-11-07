Returns the index of the first element equal to `akElement`, starting at `aiStartIndex`, or -1.
Strings compare ignoring case.

# Examples

```papyrus
String[] names = New String[3]
names[0] = "Lydia"
names[1] = "Serana"
names[2] = "Lydia"
Int first = names.Find("lydia")      ; 0
Int next = names.Find("Lydia", 1)    ; 2
Int missing = names.Find("Inigo")    ; -1
```
