#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

use crate::models::{Category, InventorySummary, Product, category_name};
pub fn find_by_category(products: Vec<Product>, cat: Category) -> Vec<Product> {
    products.filter(|p: _| (category_name(p.category) == category_name(cat)))
}

pub fn find_out_of_stock(products: Vec<Product>) -> Vec<Product> {
    products.filter(|p: _| (p.in_stock() == false))
}

pub fn find_in_stock(products: Vec<Product>) -> Vec<Product> {
    products.filter(|p: _| p.in_stock())
}

pub fn total_value(products: Vec<Product>) -> f64 {
    let values = products.map(|p: _| p.stock_value());
    values.fold(0.0_f64, |acc: _, v: _| (acc + v))
}

pub fn summarize(products: Vec<Product>) -> InventorySummary {
    let total = ((products).len() as i64);
    let value = total_value(products);
    let oos = ((find_out_of_stock(products)).len() as i64);
    InventorySummary { total_products: total, total_value: value, out_of_stock: oos }
}

pub fn format_summary(summary: InventorySummary) -> String {
    format!("Inventory: {} products, ${{summary.total_value}} total value, {} out of stock", summary.total_products, summary.out_of_stock)
}

pub fn restock(product: Product, amount: i64) -> Product {
    Product { id: product.id, name: product.name, category: product.category, price: product.price, quantity: (product.quantity + amount) }
}
