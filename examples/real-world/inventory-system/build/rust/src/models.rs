#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

use crate::service::{total_value};
#[derive(Clone)]
pub enum Category {
    Electronics,
    Clothing,
    Food,
    Books,
    Other,
}

#[derive(Clone)]
pub struct Product {
    pub id: i64,
    pub name: String,
    pub category: Category,
    pub price: f64,
    pub quantity: i64,
}

impl Product {
    pub fn in_stock(&self) -> bool {
        (self.quantity > 0_i64)
    }

    pub fn stock_value(&self) -> f64 {
        (self.price * self.quantity.to_float())
    }

    pub fn display(&self) -> String {
        format!("{} (x{}) @ ${{self.price}}", self.name, self.quantity)
    }
}

#[derive(Clone)]
pub struct InventorySummary {
    pub total_products: i64,
    pub total_value: f64,
    pub out_of_stock: i64,
}

pub fn category_name(cat: Category) -> String {
    match cat {
        Category::Electronics => "Electronics".to_string(),
        Category::Clothing => "Clothing".to_string(),
        Category::Food => "Food".to_string(),
        Category::Books => "Books".to_string(),
        Category::Other => "Other".to_string(),
    }
}
