#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

#[derive(Clone)]
pub struct DataPoint {
    pub name: String,
    pub value: f64,
    pub category: String,
}

#[derive(Clone)]
pub struct Summary {
    pub count: i64,
    pub total: f64,
    pub average: f64,
}

#[derive(Clone)]
pub struct Report {
    pub title: String,
    pub body: String,
}

pub fn normalize(data: Vec<DataPoint>) -> Vec<DataPoint> {
    let mut max_val = 0.0_f64;
    for dp in data {
        if (dp.value > max_val) {
            max_val = dp.value;
        }
    }
    if (max_val > 0.0_f64) { data.map(|dp: _| DataPoint { name: dp.name, value: (dp.value / max_val), category: dp.category }) } else { data }
}

pub fn scale(factor: f64, data: Vec<DataPoint>) -> Vec<DataPoint> {
    data.map(|dp: _| DataPoint { name: dp.name, value: (dp.value * factor), category: dp.category })
}

pub fn filter_category(cat: String) -> fn(Vec<DataPoint>) -> Vec<DataPoint> {
    |data: _| data.filter(|dp: _| (dp.category == cat))
}

pub fn remove_invalid(data: Vec<DataPoint>) -> Vec<DataPoint> {
    data.filter(|dp: _| (dp.value > 0.0_f64))
}

pub fn compute_summary(data: Vec<DataPoint>) -> Summary {
    let mut total = 0.0_f64;
    let mut count = 0_i64;
    let mut fcount = 0.0_f64;
    for dp in data {
        total = (total + dp.value);
        count = (count + 1_i64);
        fcount = (fcount + 1.0_f64);
    }
    let avg = if (fcount > 0.0_f64) { (total / fcount) } else { 0.0_f64 };
    Summary { count: count, total: total, average: avg }
}

pub fn format_summary(s: Summary) -> String {
    format!("Items: {}, Total: {}, Average: {}", s.count, s.total, s.average)
}

pub fn format_data(data: Vec<DataPoint>) -> String {
    let mut result = "".to_string();
    let mut first = true;
    for dp in data {
        if first {
            result = format!("  {}: {} [{}]", dp.name, dp.value, dp.category);
            first = false;
        } else {
            result = format!("{}{}", result, format!("
  {}: {} [{}]", dp.name, dp.value, dp.category));
        }
    }
    result
}

pub fn build_report_pipeline() -> fn(Vec<DataPoint>) -> String {
    |__compose_x: _| format_summary(|__compose_x: _| compute_summary(normalize(__compose_x))(__compose_x))
}

pub fn apply_pipeline(title: String, data: Vec<DataPoint>, pipeline: fn(Vec<DataPoint>) -> String) -> Report {
    let body = pipeline(data);
    Report { title: title, body: body }
}

pub fn print_report(report: Report) -> () {
    println!("{}", format!("--- {} ---", report.title));
    println!("{}", report.body);
    println!("{}", "".to_string())
}

fn main() {
    let data: Vec<DataPoint> = vec![DataPoint { name: "alpha".to_string(), value: 10.0_f64, category: "sensor".to_string() }, DataPoint { name: "beta".to_string(), value: 25.0_f64, category: "sensor".to_string() }, DataPoint { name: "gamma".to_string(), value: 5.0_f64, category: "manual".to_string() }, DataPoint { name: "delta".to_string(), value: 40.0_f64, category: "sensor".to_string() }, DataPoint { name: "epsilon".to_string(), value: 15.0_f64, category: "manual".to_string() }, DataPoint { name: "zeta".to_string(), value: 0.0_f64, category: "sensor".to_string() }, DataPoint { name: "eta".to_string(), value: 30.0_f64, category: "manual".to_string() }];
    println!("{}", "=== Data Pipeline Demo ===".to_string());
    println!("{}", "".to_string());
    println!("{}", "--- Raw Data Summary ---".to_string());
    let raw_summary = format_summary(compute_summary(data.clone()));
    println!("{}", raw_summary);
    println!("{}", "".to_string());
    println!("{}", "--- Cleaned + Normalized ---".to_string());
    let cleaned = normalize(remove_invalid(data.clone()));
    let cleaned_listing = format_data(cleaned);
    println!("{}", cleaned_listing);
    println!("{}", "".to_string());
    let sensor_filter = filter_category("sensor".to_string());
    let manual_filter = filter_category("manual".to_string());
    let sensor_data = sensor_filter(data.clone());
    let manual_data = manual_filter(data.clone());
    println!("{}", "--- Sensor Data Summary ---".to_string());
    let sensor_summary = format_summary(compute_summary(remove_invalid(sensor_data.clone())));
    println!("{}", sensor_summary);
    println!("{}", "".to_string());
    println!("{}", "--- Manual Data Summary ---".to_string());
    let manual_summary = format_summary(compute_summary(manual_data.clone()));
    println!("{}", manual_summary);
    println!("{}", "".to_string());
    let report_pipeline = build_report_pipeline();
    let full_report = apply_pipeline("Full Dataset Report".to_string(), data.clone(), report_pipeline.clone());
    print_report(full_report);
    let sensor_report = apply_pipeline("Sensor Report".to_string(), sensor_data.clone(), report_pipeline.clone());
    print_report(sensor_report);
    let manual_report = apply_pipeline("Manual Report".to_string(), manual_data.clone(), report_pipeline.clone());
    print_report(manual_report);
    println!("{}", "--- Scaled Sensor Summary ---".to_string());
    let scaled_result = format_summary(compute_summary(normalize(remove_invalid(sensor_data.clone()))));
    println!("{}", scaled_result);
    println!("{}", "".to_string());
    println!("{}", "=== Pipeline Complete ===".to_string())
}
