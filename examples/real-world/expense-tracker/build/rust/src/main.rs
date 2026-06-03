#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

#[derive(Clone)]
pub enum Category {
    Food,
    Transport,
    Housing,
    Entertainment,
    Utilities,
    Other,
}

#[derive(Clone)]
pub struct Expense {
    pub id: i64,
    pub amount: f64,
    pub category: Category,
    pub description: String,
    pub date: String,
}

#[derive(Clone)]
pub struct Report {
    pub total: f64,
    pub by_category: std::collections::HashMap<String, f64>,
    pub count: i64,
}

pub fn category_name(cat: Category) -> String {
    match cat {
        Category::Food => "Food".to_string(),
        Category::Transport => "Transport".to_string(),
        Category::Housing => "Housing".to_string(),
        Category::Entertainment => "Entertainment".to_string(),
        Category::Utilities => "Utilities".to_string(),
        Category::Other => "Other".to_string(),
    }
}

pub fn is_category(expense: Expense, cat: Category) -> bool {
    match expense.category {
        Category::Food => match cat {
            Category::Food => true,
            _ => false,
        },
        Category::Transport => match cat {
            Category::Transport => true,
            _ => false,
        },
        Category::Housing => match cat {
            Category::Housing => true,
            _ => false,
        },
        Category::Entertainment => match cat {
            Category::Entertainment => true,
            _ => false,
        },
        Category::Utilities => match cat {
            Category::Utilities => true,
            _ => false,
        },
        Category::Other => match cat {
            Category::Other => true,
            _ => false,
        },
    }
}

pub fn add_expense(expenses: Vec<Expense>, expense: Expense) -> Vec<Expense> {
    (expenses + vec![expense])
}

pub fn remove_expense(expenses: Vec<Expense>, id: i64) -> Vec<Expense> {
    expenses.filter(|e: _| (e.id != id))
}

pub fn find_by_category(expenses: Vec<Expense>, cat: Category) -> Vec<Expense> {
    expenses.filter(|e: _| is_category(e, cat))
}

pub fn total_spending(expenses: Vec<Expense>) -> f64 {
    let mut total = 0.0_f64;
    for e in expenses {
        total = (total + e.amount);
    }
    total
}

pub fn category_total(expenses: Vec<Expense>, cat: Category) -> f64 {
    let mut total = 0.0_f64;
    for e in expenses {
        if is_category(e, cat) {
            total = (total + e.amount);
        }
    }
    total
}

pub fn spending_by_category(expenses: Vec<Expense>) -> std::collections::HashMap<String, f64> {
    let food = category_total(expenses, Category::Food.clone());
    let transport = category_total(expenses, Category::Transport.clone());
    let housing = category_total(expenses, Category::Housing.clone());
    let entertainment = category_total(expenses, Category::Entertainment.clone());
    let utilities = category_total(expenses, Category::Utilities.clone());
    let other = category_total(expenses, Category::Other.clone());
    std::collections::HashMap::from([("Food".to_string(), food), ("Transport".to_string(), transport), ("Housing".to_string(), housing), ("Entertainment".to_string(), entertainment), ("Utilities".to_string(), utilities), ("Other".to_string(), other)])
}

pub fn generate_report(expenses: Vec<Expense>) -> Report {
    let total = total_spending(expenses);
    let by_cat = spending_by_category(expenses);
    let count = ((expenses).len() as i64);
    Report { total: total, by_category: by_cat, count: count }
}

pub fn format_report(report: Report) -> String {
    let header = "=== Expense Report ===".to_string();
    let summary = format!("Total: {} | Items: {}", report.total, report.count);
    let cat_keys = (report.by_category).keys().cloned().collect::<Vec<_>>();
    let mut lines = format!("{}{}", format!("{}{}", header, "\n".to_string()), summary);
    for key in cat_keys {
        let val = (report.by_category).get(&(key)).cloned();
        match val {
            Some(amount) => {
                lines = format!("{}{}", lines, format!("
  {}: {}", key, amount));
            }
            None => {
            }
        }
    }
    lines
}

fn main() {
    println!("{}", "=== Expense Tracker Demo ===".to_string());
    println!("{}", "".to_string());
    let mut expenses: Vec<Expense> = vec![];
    expenses = add_expense(expenses.clone(), Expense { id: 1_i64, amount: 45.50_f64, category: Category::Food, description: "Grocery shopping".to_string(), date: "2026-03-01".to_string() });
    expenses = add_expense(expenses.clone(), Expense { id: 2_i64, amount: 120.00_f64, category: Category::Housing, description: "Electric bill".to_string(), date: "2026-03-02".to_string() });
    expenses = add_expense(expenses.clone(), Expense { id: 3_i64, amount: 30.00_f64, category: Category::Transport, description: "Bus pass".to_string(), date: "2026-03-03".to_string() });
    expenses = add_expense(expenses.clone(), Expense { id: 4_i64, amount: 15.99_f64, category: Category::Entertainment, description: "Movie ticket".to_string(), date: "2026-03-05".to_string() });
    expenses = add_expense(expenses.clone(), Expense { id: 5_i64, amount: 60.00_f64, category: Category::Utilities, description: "Internet service".to_string(), date: "2026-03-06".to_string() });
    expenses = add_expense(expenses.clone(), Expense { id: 6_i64, amount: 22.75_f64, category: Category::Food, description: "Lunch out".to_string(), date: "2026-03-07".to_string() });
    println!("{}", format!("All expenses ({}):", ((expenses).len() as i64)));
    for e in expenses {
        println!("{}", format!("  #{} {}: {} [{}]", e.id, e.description, e.amount, category_name(e.category)))
    }
    expenses = remove_expense(expenses.clone(), 4_i64);
    println!("{}", "".to_string());
    println!("{}", format!("After removing #4: {} expenses", ((expenses).len() as i64)));
    let food_items = find_by_category(expenses.clone(), Category::Food);
    println!("{}", "".to_string());
    println!("{}", format!("Food expenses ({}):", ((food_items).len() as i64)));
    for e in food_items {
        println!("{}", format!("  {}: {}", e.description, e.amount))
    }
    let report = generate_report(expenses.clone());
    println!("{}", "".to_string());
    let formatted = format_report(report);
    println!("{}", formatted);
    println!("{}", "".to_string());
    println!("{}", "=== Done ===".to_string())
}
