/-
The collaborative text's model (src/crdt.rs implements it; the tests
compare the two on random histories: src/tests/crdt.rs against a Rust copy
of this walk, tests/crdt_lean.sh against this file's own definitions).

The state is a set of elements and a set of deleted ids. An element is a
character with a unique id, a parent (another element's id, or the root)
and a side. The text is the in-order walk of the tree: left children,
the element (unless deleted), right children, children in id order.

PROVED HERE (about the model, not the Rust code):
- `text_congr`: the text depends only on which elements and deletions a
  state has, not on their order or repeats.
- `converge`: two replicas that applied the same operations, in any
  order and with any repeats, show the same text (strong convergence).
- `merge_comm`, `merge_assoc`, `merge_idem`: merging states (set union)
  is commutative, associative and idempotent, up to the text.
Assumed (and enforced by the code, src/crdt.rs `check`): ids are unique,
so two different elements never share one.
-/

structure EId where
  rep : Nat
  ctr : Nat
deriving DecidableEq, Repr

def root : EId := ⟨0, 0⟩

/-- Ids in order: by replica, then counter (src/crdt.rs `key`). -/
def EId.lt (a b : EId) : Prop := a.rep < b.rep ∨ (a.rep = b.rep ∧ a.ctr < b.ctr)

instance (a b : EId) : Decidable (EId.lt a b) := by unfold EId.lt; infer_instance

theorem EId.lt_irrefl (a : EId) : ¬ EId.lt a a := by
  unfold EId.lt; omega

theorem EId.lt_trans {a b c : EId} : EId.lt a b → EId.lt b c → EId.lt a c := by
  unfold EId.lt; omega

theorem EId.lt_total {a b : EId} : a ≠ b → EId.lt a b ∨ EId.lt b a := by
  obtain ⟨ar, ac⟩ := a
  obtain ⟨br, bc⟩ := b
  intro h
  simp only [ne_eq, EId.mk.injEq] at h
  unfold EId.lt
  dsimp only
  omega

structure Elem where
  id : EId
  parent : EId
  right : Bool
  ch : Nat
deriving DecidableEq, Repr

/-- No two different elements share an id. -/
def Uniq (s : List Elem) : Prop := ∀ a ∈ s, ∀ b ∈ s, a.id = b.id → a = b

/-! ## A canonical list: sorted by id, no repeats. -/

def ins (x : Elem) : List Elem → List Elem
  | [] => [x]
  | y :: ys => if EId.lt x.id y.id then x :: y :: ys else if x = y then y :: ys else y :: ins x ys

def canon (l : List Elem) : List Elem := l.foldr ins []

def Sorted (l : List Elem) : Prop := l.Pairwise (fun a b => EId.lt a.id b.id)

theorem mem_ins {x z : Elem} {l : List Elem} : z ∈ ins x l ↔ z = x ∨ z ∈ l := by
  induction l with
  | nil => simp [ins]
  | cons y ys ih =>
    unfold ins
    split
    · simp
    · split
      · rename_i h; subst h; simp
      · simp only [List.mem_cons, ih, or_left_comm]

theorem mem_canon {z : Elem} {l : List Elem} : z ∈ canon l ↔ z ∈ l := by
  induction l with
  | nil => simp [canon]
  | cons y ys ih =>
    simp only [canon, List.foldr_cons] at *
    rw [mem_ins, ih, List.mem_cons]

theorem sorted_ins {x : Elem} {l : List Elem} (hs : Sorted l)
    (hu : ∀ b ∈ l, x.id = b.id → x = b) : Sorted (ins x l) := by
  induction l with
  | nil => simp [ins, Sorted]
  | cons y ys ih =>
    unfold Sorted at hs
    rw [List.pairwise_cons] at hs
    unfold ins
    split
    · rename_i hxy
      unfold Sorted
      rw [List.pairwise_cons]
      refine ⟨?_, ?_⟩
      · intro a ha
        rcases List.mem_cons.mp ha with rfl | ha
        · exact hxy
        · exact EId.lt_trans hxy (hs.1 a ha)
      · rw [List.pairwise_cons]; exact hs
    · split
      · unfold Sorted; rw [List.pairwise_cons]; exact hs
      · rename_i hxy hne
        have hyx : EId.lt y.id x.id := by
          rcases EId.lt_total (a := x.id) (b := y.id) (fun h => hne (hu y (by simp) h)) with h | h
          · exact absurd h hxy
          · exact h
        unfold Sorted
        rw [List.pairwise_cons]
        refine ⟨?_, ?_⟩
        · intro a ha
          rcases mem_ins.mp ha with rfl | ha
          · exact hyx
          · exact hs.1 a ha
        · exact ih hs.2 (fun b hb => hu b (by simp [hb]))

theorem sorted_canon {l : List Elem} (hu : Uniq l) : Sorted (canon l) := by
  induction l with
  | nil => simp [canon, Sorted]
  | cons y ys ih =>
    simp only [canon, List.foldr_cons]
    apply sorted_ins
    · exact ih (fun a ha b hb => hu a (by simp [ha]) b (by simp [hb]))
    · intro b hb h
      exact hu y (by simp) b (by simp [mem_canon.mp hb]) h

/-- Two sorted lists with the same members are the same list. -/
theorem sorted_eq : ∀ {l₁ l₂ : List Elem}, Sorted l₁ → Sorted l₂ →
    (∀ x, x ∈ l₁ ↔ x ∈ l₂) → l₁ = l₂
  | [], [], _, _, _ => rfl
  | [], b :: _, _, _, h => absurd ((h b).mpr (by simp)) (by simp)
  | a :: _, [], _, _, h => absurd ((h a).mp (by simp)) (by simp)
  | a :: as, b :: bs, h₁, h₂, h => by
    unfold Sorted at h₁ h₂
    rw [List.pairwise_cons] at h₁ h₂
    have hab : a = b := by
      have ha := (h a).mp (by simp)
      have hb := (h b).mpr (by simp)
      rcases List.mem_cons.mp ha with e | ha
      · exact e
      rcases List.mem_cons.mp hb with e | hb
      · exact e.symm
      have x := EId.lt_trans (h₁.1 b hb) (h₂.1 a ha)
      exact absurd x (EId.lt_irrefl _)
    subst hab
    congr 1
    apply sorted_eq h₁.2 h₂.2
    intro x
    constructor
    · intro hx
      have := (h x).mp (by simp [hx])
      rcases List.mem_cons.mp this with e | e
      · subst e; exact absurd (h₁.1 x hx) (EId.lt_irrefl _)
      · exact e
    · intro hx
      have := (h x).mpr (by simp [hx])
      rcases List.mem_cons.mp this with e | e
      · subst e; exact absurd (h₂.1 x hx) (EId.lt_irrefl _)
      · exact e

theorem canon_congr {l₁ l₂ : List Elem} (u₁ : Uniq l₁) (u₂ : Uniq l₂)
    (h : ∀ x, x ∈ l₁ ↔ x ∈ l₂) : canon l₁ = canon l₂ :=
  sorted_eq (sorted_canon u₁) (sorted_canon u₂) (fun x => by rw [mem_canon, mem_canon, h])

/-! ## The text -/

structure State where
  elems : List Elem
  dels : List EId

/-- The in-order walk under element e (fuel: the tree's depth is at most
the number of elements; a malformed cycle just ends). -/
def walk (s : List Elem) (del : EId → Bool) : Nat → Elem → List Nat
  | 0, _ => []
  | n + 1, e =>
    (s.filter (fun c => c.parent = e.id ∧ c.right = false)).flatMap (walk s del n) ++
    (if del e.id then [] else [e.ch]) ++
    (s.filter (fun c => c.parent = e.id ∧ c.right = true)).flatMap (walk s del n)

def text (st : State) : List Nat :=
  let s := canon st.elems
  (s.filter (fun c => c.parent = root ∧ c.right = true)).flatMap
    (walk s (fun i => st.dels.contains i) (s.length + 1))

/-- THE TEXT DEPENDS ONLY ON THE SETS. -/
theorem text_congr (a b : State) (ua : Uniq a.elems) (ub : Uniq b.elems)
    (he : ∀ x, x ∈ a.elems ↔ x ∈ b.elems) (hd : ∀ i, i ∈ a.dels ↔ i ∈ b.dels) :
    text a = text b := by
  have hc := canon_congr ua ub he
  have hdel : (fun i => a.dels.contains i) = (fun i => b.dels.contains i) := by
    funext i
    rw [Bool.eq_iff_iff, List.contains_iff_mem, List.contains_iff_mem]
    exact hd i
  unfold text
  simp only [hc, hdel]

/-! ## Operations and merging -/

inductive Op where
  /-- A run: elements (rep, ctr), (rep, ctr + 1), … for chars; the first
  a child of parent on the side, each next the right child of the one
  before (src/crdt.rs `Op::Ins`). -/
  | ins (rep ctr : Nat) (parent : EId) (right : Bool) (chars : List Nat)
  /-- Deletes (rep, ctr) … (rep, ctr + len - 1). -/
  | del (rep ctr len : Nat)
deriving DecidableEq

def runElems (rep : Nat) : Nat → EId → Bool → List Nat → List Elem
  | _, _, _, [] => []
  | ctr, p, r, c :: cs => ⟨⟨rep, ctr⟩, p, r, c⟩ :: runElems rep (ctr + 1) ⟨rep, ctr⟩ true cs

def Op.elems : Op → List Elem
  | .ins rep ctr p r cs => runElems rep ctr p r cs
  | .del .. => []

def Op.dels : Op → List EId
  | .ins .. => []
  | .del rep ctr len => (List.range len).map (fun k => ⟨rep, ctr + k⟩)

def apply (st : State) (o : Op) : State := ⟨st.elems ++ o.elems, st.dels ++ o.dels⟩

def applyAll (st : State) (os : List Op) : State := os.foldl apply st

theorem applyAll_elems (st : State) (os : List Op) (x : Elem) :
    x ∈ (applyAll st os).elems ↔ x ∈ st.elems ∨ ∃ o ∈ os, x ∈ o.elems := by
  induction os generalizing st with
  | nil => simp [applyAll]
  | cons o os ih =>
    simp only [applyAll, List.foldl_cons] at *
    rw [ih]
    simp only [apply, List.mem_append, List.mem_cons]
    constructor
    · rintro ((h | h) | ⟨o', h1, h2⟩)
      · exact Or.inl h
      · exact Or.inr ⟨o, Or.inl rfl, h⟩
      · exact Or.inr ⟨o', Or.inr h1, h2⟩
    · rintro (h | ⟨o', (rfl | h1), h2⟩)
      · exact Or.inl (Or.inl h)
      · exact Or.inl (Or.inr h2)
      · exact Or.inr ⟨o', h1, h2⟩

theorem applyAll_dels (st : State) (os : List Op) (x : EId) :
    x ∈ (applyAll st os).dels ↔ x ∈ st.dels ∨ ∃ o ∈ os, x ∈ o.dels := by
  induction os generalizing st with
  | nil => simp [applyAll]
  | cons o os ih =>
    simp only [applyAll, List.foldl_cons] at *
    rw [ih]
    simp only [apply, List.mem_append, List.mem_cons]
    constructor
    · rintro ((h | h) | ⟨o', h1, h2⟩)
      · exact Or.inl h
      · exact Or.inr ⟨o, Or.inl rfl, h⟩
      · exact Or.inr ⟨o', Or.inr h1, h2⟩
    · rintro (h | ⟨o', (rfl | h1), h2⟩)
      · exact Or.inl (Or.inl h)
      · exact Or.inl (Or.inr h2)
      · exact Or.inr ⟨o', h1, h2⟩

def empty : State := ⟨[], []⟩

/-- CONVERGENCE: the same operations, in any order and with any repeats,
give the same text (the ids in them unique). -/
theorem converge (os₁ os₂ : List Op) (h : ∀ o, o ∈ os₁ ↔ o ∈ os₂)
    (u₁ : Uniq (applyAll empty os₁).elems) (u₂ : Uniq (applyAll empty os₂).elems) :
    text (applyAll empty os₁) = text (applyAll empty os₂) := by
  apply text_congr _ _ u₁ u₂
  · intro x
    rw [applyAll_elems, applyAll_elems]
    simp only [empty, List.not_mem_nil, false_or]
    constructor
    · rintro ⟨o, ho, hx⟩; exact ⟨o, (h o).mp ho, hx⟩
    · rintro ⟨o, ho, hx⟩; exact ⟨o, (h o).mpr ho, hx⟩
  · intro x
    rw [applyAll_dels, applyAll_dels]
    simp only [empty, List.not_mem_nil, false_or]
    constructor
    · rintro ⟨o, ho, hx⟩; exact ⟨o, (h o).mp ho, hx⟩
    · rintro ⟨o, ho, hx⟩; exact ⟨o, (h o).mpr ho, hx⟩

def merge (a b : State) : State := ⟨a.elems ++ b.elems, a.dels ++ b.dels⟩

theorem merge_comm (a b : State) (u : Uniq (merge a b).elems) :
    text (merge a b) = text (merge b a) := by
  have u' : Uniq (merge b a).elems := fun x hx y hy =>
    u x (by simp only [merge, List.mem_append] at *; exact hx.symm) y
      (by simp only [merge, List.mem_append] at *; exact hy.symm)
  apply text_congr _ _ u u' <;> intro x <;> simp only [merge, List.mem_append, or_comm]

theorem merge_assoc (a b c : State) (u : Uniq (merge (merge a b) c).elems) :
    text (merge (merge a b) c) = text (merge a (merge b c)) := by
  have u' : Uniq (merge a (merge b c)).elems := fun x hx y hy =>
    u x (by simp only [merge, List.mem_append, or_assoc] at *; exact hx) y
      (by simp only [merge, List.mem_append, or_assoc] at *; exact hy)
  apply text_congr _ _ u u' <;> intro x <;> simp only [merge, List.mem_append, or_assoc]

theorem merge_idem (a : State) (u : Uniq a.elems) : text (merge a a) = text a := by
  have u' : Uniq (merge a a).elems := fun x hx y hy =>
    u x (by simp only [merge, List.mem_append, or_self] at hx; exact hx) y
      (by simp only [merge, List.mem_append, or_self] at hy; exact hy)
  apply text_congr _ _ u' u <;> intro x <;> simp only [merge, List.mem_append, or_self]
