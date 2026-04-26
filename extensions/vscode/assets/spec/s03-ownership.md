# Spec Excerpt: Ownership Model

## Core Rules
1. Values are owned — every value has one owner.
2. Ownership transfers on assignment (move semantics).
3. Borrowing is implicit for reads.
4. Explicit `mut` for mutable borrows.
5. No lifetime annotations — compiler infers or errors.

## Examples
```bock
let data = load_records()       // data owns
let summary = summarize(data)   // implicit borrow
transform(mut data)             // mutable borrow
let archive = data              // ownership moves
// use(data)                    // error: moved
```

## Target Mapping
| Bock           | Rust          | GC Targets     | C++           |
|----------------|---------------|----------------|---------------|
| Ownership      | Direct        | Ignored (GC)   | std::move     |
| Immutable borrow| &T           | Pass by ref    | const T&      |
| Mutable borrow | &mut T        | Pointer/ref    | T&            |
| Move           | Move semantics| Reassignment   | std::move     |

## @managed Escape Hatch
GC semantics regardless of target. For UI/prototype code.

## Control Flow and Ownership
At if/else and match join points, diverging branches (return,
break, continue, Never-typed) are excluded from ownership state
merging. Non-diverging branches merge conservatively: if any
non-diverging branch moves a variable, it's considered moved
at the join point. Moving inside a loop body is an error.
See P4.4 package for full rules and examples.

## AIR Representation
```
OwnershipInfo {
  state: Owned | Borrowed | Moved | Managed
  mutable: Bool
  origin: NodeId        // where ownership was established
  borrows: List[BorrowInfo]
}
```
