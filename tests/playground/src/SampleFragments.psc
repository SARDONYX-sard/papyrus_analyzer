; #![allow(all)]
; A fragment of the Creation Kit, as it generates them. Hover the markers
; (;BEGIN FRAGMENT CODE, ;BEGIN CODE...): they have the color of attributes.
; Format this file: nothing changes until ;END FRAGMENT CODE, even the odd code
; below; no code action or `lint --fix` changes it either (try the problem of
; `abDone == True`, with the first line commented out: only "Allow in this file").
;BEGIN FRAGMENT CODE - Do not edit anything between this and the end comment
;NEXT FRAGMENT INDEX 2
Scriptname   SampleFragments Extends Quest Hidden

;BEGIN ALIAS PROPERTY Player
;ALIAS PROPERTY TYPE ReferenceAlias
ReferenceAlias Property Alias_Player Auto
;END ALIAS PROPERTY

;BEGIN FRAGMENT Fragment_1
Function Fragment_1()
;BEGIN AUTOCAST TYPE Fragments
Quest __temp = self as Quest
SampleFragments kmyQuest = __temp as SampleFragments
;END AUTOCAST
;BEGIN CODE
	if(kmyQuest.abDone==True)


        setobjectivedisplayed( 10 )
endif
;END CODE
EndFunction
;END FRAGMENT

;END FRAGMENT CODE - Do not edit anything between this and the begin comment

; Below the fragment: formatted and fixed as usual. The next comment is a plain
; one here: the color of a comment, no hover.
; END CODE
Bool Property abDone = False Auto
