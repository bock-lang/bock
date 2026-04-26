fn greet(name: String) -> String {
    format!("Hello, {}!", name)
}

fn banner(title: String, subtitle: String) -> String {
    format!("=== {} === {}", title, subtitle)
}

fn main() {
    println!("{}", "Hello, World!".to_string());
    let name = "Bock".to_string();
    println!("{}", format!("Welcome to {}!", name));
    println!("{}", greet("World".to_string()));
    println!("{}", greet("Bock Developer".to_string()));
    let heading = banner("Bock Language".to_string(), "Simple. Declarative. Powerful.".to_string());
    println!("{}", heading);
    let language = "Bock".to_string();
    let version = "0.1.0".to_string();
    println!("{}", format!("{} v{} — ready to go!", language, version))
}
