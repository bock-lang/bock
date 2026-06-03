import { totalValue } from "./service.js";
export type Category = Category_Electronics | Category_Clothing | Category_Food | Category_Books | Category_Other;

interface Category_Electronics { readonly _tag: "Electronics"; }
const Category_Electronics: Category_Electronics = Object.freeze({ _tag: "Electronics" as const });
interface Category_Clothing { readonly _tag: "Clothing"; }
const Category_Clothing: Category_Clothing = Object.freeze({ _tag: "Clothing" as const });
interface Category_Food { readonly _tag: "Food"; }
const Category_Food: Category_Food = Object.freeze({ _tag: "Food" as const });
interface Category_Books { readonly _tag: "Books"; }
const Category_Books: Category_Books = Object.freeze({ _tag: "Books" as const });
interface Category_Other { readonly _tag: "Other"; }
const Category_Other: Category_Other = Object.freeze({ _tag: "Other" as const });

export class Product {
  id: number;
  name: string;
  category: Category;
  price: number;
  quantity: number;
  constructor({ id, name, category, price, quantity }: { id: number; name: string; category: Category; price: number; quantity: number }) {
    this.id = id;
    this.name = name;
    this.category = category;
    this.price = price;
    this.quantity = quantity;
  }
}

export interface Product {
  in_stock(self: Product): boolean;
  stock_value(self: Product): number;
  display(self: Product): string;
}
// impl Product
Product.prototype.in_stock = function(self: Product): boolean {
  return (self.quantity > 0);
};
Product.prototype.stock_value = function(self: Product): number {
  return (self.price * self.quantity.to_float(self.quantity));
};
Product.prototype.display = function(self: Product): string {
  return `${self.name} (x${self.quantity}) @ \${self.price}`;
};

export class InventorySummary {
  total_products: number;
  total_value: number;
  out_of_stock: number;
  constructor({ total_products, total_value, out_of_stock }: { total_products: number; total_value: number; out_of_stock: number }) {
    this.total_products = total_products;
    this.total_value = total_value;
    this.out_of_stock = out_of_stock;
  }
}

export function categoryName(cat: Category): string {
  return (() => {
    switch (cat._tag) {
      case "Electronics": {
        return "Electronics";
        break;
      }
      case "Clothing": {
        return "Clothing";
        break;
      }
      case "Food": {
        return "Food";
        break;
      }
      case "Books": {
        return "Books";
        break;
      }
      case "Other": {
        return "Other";
        break;
      }
    }
  })();
}
export { Category_Books, Category_Clothing, Category_Electronics, Category_Food, Category_Other };
//# sourceMappingURL=models.ts.map
