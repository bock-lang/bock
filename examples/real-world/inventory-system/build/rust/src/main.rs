#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

mod models;
mod service;

use crate::models::{Category, Product};
use crate::service::{find_by_category, find_out_of_stock, format_summary, restock, summarize};
fn main() {
    let products = vec![Product { id: 1_i64, name: "Laptop".to_string(), category: Category::Electronics, price: 999.99_f64, quantity: 5_i64 }, Product { id: 2_i64, name: "T-Shirt".to_string(), category: Category::Clothing, price: 19.99_f64, quantity: 100_i64 }, Product { id: 3_i64, name: "Rice".to_string(), category: Category::Food, price: 4.50_f64, quantity: 0_i64 }, Product { id: 4_i64, name: "Novel".to_string(), category: Category::Books, price: 12.99_f64, quantity: 25_i64 }, Product { id: 5_i64, name: "Headphones".to_string(), category: Category::Electronics, price: 49.99_f64, quantity: 0_i64 }];
    println!("{}", "=== Inventory ===".to_string());
    for p in products {
        println!("{}", p.display())
    }
    let summary = summarize(products.clone());
    println!("{}", "".to_string());
    println!("{}", format_summary(summary));
    let electronics = find_by_category(products.clone(), Category::Electronics.clone());
    println!("{}", "".to_string());
    println!("{}", format!("=== Electronics ({}) ===", ((electronics).len() as i64)));
    for p in electronics {
        println!("{}", p.display())
    }
    let oos = find_out_of_stock(products.clone());
    println!("{}", "".to_string());
    println!("{}", format!("=== Out of Stock ({}) ===", ((oos).len() as i64)));
    for p in oos {
        println!("{}", p.display())
    }
    let rice = Product { id: 3_i64, name: "Rice".to_string(), category: Category::Food, price: 4.50_f64, quantity: 0_i64 };
    let restocked = restock(rice, 50_i64);
    println!("{}", "".to_string());
    println!("{}", format!("Restocked: {}", restocked.display()))
}
