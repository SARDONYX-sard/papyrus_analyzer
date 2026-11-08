; `papyrus_analyzer test` compiles and runs the examples of the documentation
; comments. Write them in `;` comments: `{ }` comments are shown in the Creation Kit.
; Lines starting with `# ` are for the test only: hover `Add`, they are not shown.
;
; # Examples
;
; ```papyrus
; Int sum = DocTests.Add(1, 2)
; ```
ScriptName SampleDocTests
{Doc tests, like rustdoc.}

; # Examples
;
; ```papyrus
; # Int expected = 3
; AssertIntEq(Add(1, 2), expected)
; ```
;
; Compiled only, `no_run`:
;
; ```papyrus,no_run
; Game.QuitToMainMenu()
; ```
;
; Not even compiled, `ignore`:
;
; ```papyrus,ignore
; Int x = Add(1)
; ```
Int Function Add(Int a, Int b) Global
{The sum of a and b.}
    Return a + b
EndFunction
