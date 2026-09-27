/- Appended to Fugue.lean by tests/crdt_lean.sh: reads operations (one a
line, as src/tests/crdt.rs lean_case prints them) and prints the model's
text as code points. -/
def parseOp (l : String) : Option Op :=
  match l.splitOn " " with
  | ["i", r, c, pr, pc, s, cs] => do
    let chars ← (cs.splitOn ",").mapM String.toNat?
    pure (.ins (← r.toNat?) (← c.toNat?) ⟨← pr.toNat?, ← pc.toNat?⟩ (s == "1") chars)
  | ["d", r, c, n] => do pure (.del (← r.toNat?) (← c.toNat?) (← n.toNat?))
  | _ => none

def main : IO Unit := do
  let stdin ← IO.getStdin
  let mut ops : List Op := []
  repeat
    let l ← stdin.getLine
    if l.isEmpty then break
    let l := l.trimRight
    if l.startsWith "=" then continue
    match parseOp l with
    | some o => ops := ops ++ [o]
    | none => throw (IO.userError s!"bad line {l}")
  let t := text (applyAll empty ops)
  IO.println ("= " ++ " ".intercalate (t.map toString))
