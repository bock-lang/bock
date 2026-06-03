from __future__ import annotations

from models import *
from service import *
from models import Product
from service import find_by_category, find_out_of_stock, format_summary, restock, summarize
def main():
    products = [Product(id=1, name="Laptop", category=Category_Electronics(), price=999.99, quantity=5), Product(id=2, name="T-Shirt", category=Category_Clothing(), price=19.99, quantity=100), Product(id=3, name="Rice", category=Category_Food(), price=4.50, quantity=0), Product(id=4, name="Novel", category=Category_Books(), price=12.99, quantity=25), Product(id=5, name="Headphones", category=Category_Electronics(), price=49.99, quantity=0)]
    print("=== Inventory ===")
    for p in products:
        return print(p.display())
    summary = summarize(products)
    print("")
    print(format_summary(summary))
    electronics = find_by_category(products, Category_Electronics())
    print("")
    print(f"=== Electronics ({len(electronics)}) ===")
    for p in electronics:
        return print(p.display())
    oos = find_out_of_stock(products)
    print("")
    print(f"=== Out of Stock ({len(oos)}) ===")
    for p in oos:
        return print(p.display())
    rice = Product(id=3, name="Rice", category=Category_Food(), price=4.50, quantity=0)
    restocked = restock(rice, 50)
    print("")
    return print(f"Restocked: {restocked.display()}")
if __name__ == "__main__":
    main()
