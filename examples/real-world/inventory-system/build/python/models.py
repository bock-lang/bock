from __future__ import annotations
from typing import Union
from dataclasses import dataclass

from service import total_value
@dataclass(frozen=True)
class Category_Electronics:
    _tag: str = "Electronics"

@dataclass(frozen=True)
class Category_Clothing:
    _tag: str = "Clothing"

@dataclass(frozen=True)
class Category_Food:
    _tag: str = "Food"

@dataclass(frozen=True)
class Category_Books:
    _tag: str = "Books"

@dataclass(frozen=True)
class Category_Other:
    _tag: str = "Other"
Category = Union[Category_Electronics, Category_Clothing, Category_Food, Category_Books, Category_Other]

@dataclass
class Product:
    id: int
    name: str
    category: Category
    price: float
    quantity: int

    def in_stock(self) -> bool:
        return (self.quantity > 0)

    def stock_value(self) -> float:
        return (self.price * self.quantity.to_float())

    def display(self) -> str:
        return f"{self.name} (x{self.quantity}) @ ${{self.price}}"

@dataclass
class InventorySummary:
    total_products: int
    total_value: float
    out_of_stock: int

def category_name(cat: Category) -> str:
    return (lambda __v: "Electronics" if isinstance(__v, Category_Electronics) else ("Clothing" if isinstance(__v, Category_Clothing) else ("Food" if isinstance(__v, Category_Food) else ("Books" if isinstance(__v, Category_Books) else ("Other")))))(cat)
