from __future__ import annotations
from _bock_runtime import *
from typing import Union
from dataclasses import dataclass

@dataclass(frozen=True)
class Category_Food:
    _tag: str = "Food"

@dataclass(frozen=True)
class Category_Transport:
    _tag: str = "Transport"

@dataclass(frozen=True)
class Category_Housing:
    _tag: str = "Housing"

@dataclass(frozen=True)
class Category_Entertainment:
    _tag: str = "Entertainment"

@dataclass(frozen=True)
class Category_Utilities:
    _tag: str = "Utilities"

@dataclass(frozen=True)
class Category_Other:
    _tag: str = "Other"
Category = Union[Category_Food, Category_Transport, Category_Housing, Category_Entertainment, Category_Utilities, Category_Other]

@dataclass
class Expense:
    id: int
    amount: float
    category: Category
    description: str
    date: str

@dataclass
class Report:
    total: float
    by_category: dict[str, float]
    count: int

def category_name(cat: Category) -> str:
    return (lambda __v: "Food" if isinstance(__v, Category_Food) else ("Transport" if isinstance(__v, Category_Transport) else ("Housing" if isinstance(__v, Category_Housing) else ("Entertainment" if isinstance(__v, Category_Entertainment) else ("Utilities" if isinstance(__v, Category_Utilities) else ("Other"))))))(cat)

def is_category(expense: Expense, cat: Category) -> bool:
    return (lambda __v: (lambda __v: True if isinstance(__v, Category_Food) else (False))(cat) if isinstance(__v, Category_Food) else ((lambda __v: True if isinstance(__v, Category_Transport) else (False))(cat) if isinstance(__v, Category_Transport) else ((lambda __v: True if isinstance(__v, Category_Housing) else (False))(cat) if isinstance(__v, Category_Housing) else ((lambda __v: True if isinstance(__v, Category_Entertainment) else (False))(cat) if isinstance(__v, Category_Entertainment) else ((lambda __v: True if isinstance(__v, Category_Utilities) else (False))(cat) if isinstance(__v, Category_Utilities) else ((lambda __v: True if isinstance(__v, Category_Other) else (False))(cat)))))))(expense.category)

def add_expense(expenses: list[Expense], expense: Expense) -> list[Expense]:
    return (expenses + [expense])

def remove_expense(expenses: list[Expense], id: int) -> list[Expense]:
    return expenses.filter(lambda e: (e.id != id))

def find_by_category(expenses: list[Expense], cat: Category) -> list[Expense]:
    return expenses.filter(lambda e: is_category(e, cat))

def total_spending(expenses: list[Expense]) -> float:
    total = 0.0
    for e in expenses:
        total = (total + e.amount)
    return total

def category_total(expenses: list[Expense], cat: Category) -> float:
    total = 0.0
    for e in expenses:
        if is_category(e, cat):
            total = (total + e.amount)
    return total

def spending_by_category(expenses: list[Expense]) -> dict[str, float]:
    food = category_total(expenses, Category_Food())
    transport = category_total(expenses, Category_Transport())
    housing = category_total(expenses, Category_Housing())
    entertainment = category_total(expenses, Category_Entertainment())
    utilities = category_total(expenses, Category_Utilities())
    other = category_total(expenses, Category_Other())
    return {"Food": food, "Transport": transport, "Housing": housing, "Entertainment": entertainment, "Utilities": utilities, "Other": other}

def generate_report(expenses: list[Expense]) -> Report:
    total = total_spending(expenses)
    by_cat = spending_by_category(expenses)
    count = len(expenses)
    return Report(total=total, by_category=by_cat, count=count)

def format_report(report: Report) -> str:
    header = "=== Expense Report ==="
    summary = f"Total: {report.total} | Items: {report.count}"
    cat_keys = list(report.by_category.keys())
    lines = ((header + "\n") + summary)
    for key in cat_keys:
        val = (lambda __m, __k: _BockSome(__m[__k]) if __k in __m else _bock_none)(report.by_category, key)
        match val:
            case _BockSome(amount):
                lines = (lines + f"""
  {key}: {amount}""")
            case _BockNone():
                pass
    return lines

def main():
    print("=== Expense Tracker Demo ===")
    print("")
    expenses: list[Expense] = []
    expenses = add_expense(expenses, Expense(id=1, amount=45.50, category=Category_Food(), description="Grocery shopping", date="2026-03-01"))
    expenses = add_expense(expenses, Expense(id=2, amount=120.00, category=Category_Housing(), description="Electric bill", date="2026-03-02"))
    expenses = add_expense(expenses, Expense(id=3, amount=30.00, category=Category_Transport(), description="Bus pass", date="2026-03-03"))
    expenses = add_expense(expenses, Expense(id=4, amount=15.99, category=Category_Entertainment(), description="Movie ticket", date="2026-03-05"))
    expenses = add_expense(expenses, Expense(id=5, amount=60.00, category=Category_Utilities(), description="Internet service", date="2026-03-06"))
    expenses = add_expense(expenses, Expense(id=6, amount=22.75, category=Category_Food(), description="Lunch out", date="2026-03-07"))
    print(f"All expenses ({len(expenses)}):")
    for e in expenses:
        return print(f"  #{e.id} {e.description}: {e.amount} [{category_name(e.category)}]")
    expenses = remove_expense(expenses, 4)
    print("")
    print(f"After removing #4: {len(expenses)} expenses")
    food_items = find_by_category(expenses, Category_Food())
    print("")
    print(f"Food expenses ({len(food_items)}):")
    for e in food_items:
        return print(f"  {e.description}: {e.amount}")
    report = generate_report(expenses)
    print("")
    formatted = format_report(report)
    print(formatted)
    print("")
    return print("=== Done ===")
if __name__ == "__main__":
    main()
