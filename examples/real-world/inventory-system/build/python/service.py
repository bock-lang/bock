from __future__ import annotations

from models import *
from models import Category, InventorySummary, Product, category_name
def find_by_category(products: list[Product], cat: Category) -> list[Product]:
    return products.filter(lambda p: (category_name(p.category) == category_name(cat)))

def find_out_of_stock(products: list[Product]) -> list[Product]:
    return products.filter(lambda p: (p.in_stock() == False))

def find_in_stock(products: list[Product]) -> list[Product]:
    return products.filter(lambda p: p.in_stock())

def total_value(products: list[Product]) -> float:
    values = products.map(lambda p: p.stock_value())
    return values.fold(0.0, lambda acc, v: (acc + v))

def summarize(products: list[Product]) -> InventorySummary:
    total = len(products)
    value = total_value(products)
    oos = len(find_out_of_stock(products))
    return InventorySummary(total_products=total, total_value=value, out_of_stock=oos)

def format_summary(summary: InventorySummary) -> str:
    return f"Inventory: {summary.total_products} products, ${{summary.total_value}} total value, {summary.out_of_stock} out of stock"

def restock(product: Product, amount: int) -> Product:
    return Product(id=product.id, name=product.name, category=product.category, price=product.price, quantity=(product.quantity + amount))
