import { InventorySummary, Product, categoryName } from "./models.js";
export function findByCategory(products, cat) {
  return products.filter(products, (p) => (categoryName(p.category) === categoryName(cat)));
}

export function findOutOfStock(products) {
  return products.filter(products, (p) => (p.in_stock(p) === false));
}

export function findInStock(products) {
  return products.filter(products, (p) => p.in_stock(p));
}

export function totalValue(products) {
  const values = products.map(products, (p) => p.stock_value(p));
  return values.fold(values, 0.0, (acc, v) => (acc + v));
}

export function summarize(products) {
  const total = (products).length;
  const value = totalValue(products);
  const oos = (findOutOfStock(products)).length;
  return new InventorySummary({ total_products: total, total_value: value, out_of_stock: oos });
}

export function formatSummary(summary) {
  return `Inventory: ${summary.total_products} products, \${summary.total_value} total value, ${summary.out_of_stock} out of stock`;
}

export function restock(product, amount) {
  return new Product({ id: product.id, name: product.name, category: product.category, price: product.price, quantity: (product.quantity + amount) });
}
//# sourceMappingURL=service.js.map
