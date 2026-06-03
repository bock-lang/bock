import { Product } from "./models.js";
import { findByCategory, findOutOfStock, formatSummary, restock, summarize } from "./service.js";
function main() {
  const products = [new Product({ id: 1, name: "Laptop", category: Category_Electronics, price: 999.99, quantity: 5 }), new Product({ id: 2, name: "T-Shirt", category: Category_Clothing, price: 19.99, quantity: 100 }), new Product({ id: 3, name: "Rice", category: Category_Food, price: 4.50, quantity: 0 }), new Product({ id: 4, name: "Novel", category: Category_Books, price: 12.99, quantity: 25 }), new Product({ id: 5, name: "Headphones", category: Category_Electronics, price: 49.99, quantity: 0 })];
  console.log("=== Inventory ===");
  for (const p of products) {
    return console.log(p.display(p));
  }
  const summary = summarize(products);
  console.log("");
  console.log(formatSummary(summary));
  const electronics = findByCategory(products, Category_Electronics);
  console.log("");
  console.log(`=== Electronics (${(electronics).length}) ===`);
  for (const p of electronics) {
    return console.log(p.display(p));
  }
  const oos = findOutOfStock(products);
  console.log("");
  console.log(`=== Out of Stock (${(oos).length}) ===`);
  for (const p of oos) {
    return console.log(p.display(p));
  }
  const rice = new Product({ id: 3, name: "Rice", category: Category_Food, price: 4.50, quantity: 0 });
  const restocked = restock(rice, 50);
  console.log("");
  return console.log(`Restocked: ${restocked.display(restocked)}`);
}
main();
//# sourceMappingURL=main.js.map
