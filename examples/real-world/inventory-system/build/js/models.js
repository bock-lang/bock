import { totalValue } from "./service.js";
const Category_Electronics = Object.freeze({ _tag: "Electronics" });
const Category_Clothing = Object.freeze({ _tag: "Clothing" });
const Category_Food = Object.freeze({ _tag: "Food" });
const Category_Books = Object.freeze({ _tag: "Books" });
const Category_Other = Object.freeze({ _tag: "Other" });

class Product {
  constructor({ id, name, category, price, quantity }) {
    this.id = id;
    this.name = name;
    this.category = category;
    this.price = price;
    this.quantity = quantity;
  }
}

// impl Product
Product.prototype.in_stock = function(self) {
  return (self.quantity > 0);
};
Product.prototype.stock_value = function(self) {
  return (self.price * self.quantity.to_float(self.quantity));
};
Product.prototype.display = function(self) {
  return `${self.name} (x${self.quantity}) @ \${self.price}`;
};

class InventorySummary {
  constructor({ total_products, total_value, out_of_stock }) {
    this.total_products = total_products;
    this.total_value = total_value;
    this.out_of_stock = out_of_stock;
  }
}

export function categoryName(cat) {
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
export { Category_Books, Category_Clothing, Category_Electronics, Category_Food, Category_Other, InventorySummary, Product };
//# sourceMappingURL=models.js.map
