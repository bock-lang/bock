#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]

struct Resource {
    pub name: String,
    pub data: Vec<i64>,
}

impl Resource {
    fn describe(&self, self: _) -> String {
        format!("Resource({}, len={})", self.name, self.data.len(self.data))
    }
}

enum ParseResult {
    Parsed(String),
    Failed(String),
}

fn move_basics() -> () {
    println!("{}", "--- Move Semantics ---".to_string());
    let a = vec![1_i64, 2_i64, 3_i64];
    let b = a;
    println!("{}", format!("b has {} items", b.len(b)));
    let x = Resource { name: "config".to_string(), data: vec![10_i64, 20_i64] };
    let y = x;
    let z = y;
    println!("{}", format!("z = {}", z.describe(z)))
}

fn count_items(items: Vec<i64>) -> i64 {
    items.len(items)
}

fn describe_resource(r: Resource) -> String {
    r.describe(r)
}

fn implicit_borrow_demo() -> () {
    println!("{}", "--- Implicit Borrow ---".to_string());
    let data = vec![10_i64, 20_i64, 30_i64, 40_i64, 50_i64];
    let n = count_items(data);
    println!("{}", format!("count = {}", n));
    let n2 = count_items(data);
    println!("{}", format!("count again = {}", n2));
    let n3 = data.len(data);
    println!("{}", format!("and once more = {}", n3));
    let res = Resource { name: "db".to_string(), data: vec![1_i64, 2_i64, 3_i64] };
    let desc = describe_resource(res);
    println!("{}", format!("desc = {}", desc));
    let desc2 = describe_resource(res);
    println!("{}", format!("desc again = {}", desc2))
}

fn append_item(items: Vec<i64>, value: i64) -> Vec<i64> {
    items = (items + vec![value]);
    items
}

fn double_all(items: Vec<i64>) -> Vec<i64> {
    items = items.map(items, |x: _| (x * 2_i64));
    items
}

fn mutable_borrow_demo() -> () {
    println!("{}", "--- Mutable Borrow ---".to_string());
    let mut nums = vec![1_i64, 2_i64, 3_i64];
    let result = append_item(nums, 4_i64);
    println!("{}", format!("appended: len={}", result.len(result)));
    let mut vals = vec![10_i64, 20_i64, 30_i64];
    let doubled = double_all(vals);
    println!("{}", format!("doubled: len={}", doubled.len(doubled)))
}

fn build_report() -> String {
    let title = "Quarterly Report".to_string();
    let header = format!("=== {} ===", title);
    let section1 = format!("Section 1 of {}", title);
    let section2 = format!("Section 2 of {}", title);
    let footer = format!("End of {}", title);
    format!("{} | {} | {} | {}", header, section1, section2, footer)
}

fn build_ui_tree() -> String {
    let app_name = "Bock App".to_string();
    let theme = "dark".to_string();
    let header = format!("Header: {} ({})", app_name, theme);
    let sidebar = format!("Sidebar: {} nav", app_name);
    let content = format!("Content: {} main", app_name);
    let footer = format!("Footer: {} v1.0", app_name);
    format!("{} | {} | {} | {}", header, sidebar, content, footer)
}

fn managed_demo() -> () {
    println!("{}", "--- @managed Escape Hatch ---".to_string());
    let report = build_report();
    println!("{}", format!("report: {}", report));
    let ui = build_ui_tree();
    println!("{}", format!("ui: {}", ui))
}

fn validate(input: String) -> Result<String, String> {
    if (input == "".to_string()) { Err("empty input".to_string()) } else { Ok(input) }
}

fn guard_ownership_demo() -> () {
    println!("{}", "--- Guard + Ownership ---".to_string());
    if !(validate("hello".to_string())) {
        /* unsupported */
    }
    println!("{}", format!("validated: {}", val));
    if !(validate("world".to_string())) {
        /* unsupported */
    }
    println!("{}", format!("also validated: {}", val2))
}

fn classify(n: i64) -> String {
    let label = match n {
        0_i64 => "zero".to_string(),
        1_i64 => "one".to_string(),
        _ => /* unsupported */,
    };
    format!("classified as: {}", label)
}

fn find_first_positive(items: Vec<i64>) -> String {
    let result = match items {
        [] => /* unsupported */,
        [first, rest @ ..] => first,
    };
    format!("first element: {}", result)
}

fn never_demo() -> () {
    println!("{}", "--- Match with Never ---".to_string());
    println!("{}", classify(0_i64));
    println!("{}", classify(1_i64));
    println!("{}", classify(42_i64));
    println!("{}", find_first_positive(vec![]));
    println!("{}", find_first_positive(vec![7_i64, 8_i64, 9_i64]))
}

fn would_fail_examples() -> () {
    println!("{}", "--- What Would Fail (commented-out examples) ---".to_string());
    println!("{}", "(See source comments for ownership error examples)".to_string())
}

fn main() {
    println!("{}", "=== Ownership Demo ===".to_string());
    println!("{}", "".to_string());
    move_basics();
    println!("{}", "".to_string());
    implicit_borrow_demo();
    println!("{}", "".to_string());
    mutable_borrow_demo();
    println!("{}", "".to_string());
    managed_demo();
    println!("{}", "".to_string());
    guard_ownership_demo();
    println!("{}", "".to_string());
    never_demo();
    println!("{}", "".to_string());
    would_fail_examples();
    println!("{}", "".to_string());
    println!("{}", "=== Ownership Demo Complete ===".to_string())
}
