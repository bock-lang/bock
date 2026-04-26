def greet(name: str) -> str:
    return f"Hello, {name}!"

def banner(title: str, subtitle: str) -> str:
    return f"=== {title} === {subtitle}"

def main():
    print("Hello, World!")
    name = "Bock"
    print(f"Welcome to {name}!")
    print(greet("World"))
    print(greet("Bock Developer"))
    heading = banner("Bock Language", "Simple. Declarative. Powerful.")
    print(heading)
    language = "Bock"
    version = "0.1.0"
    return print(f"{language} v{version} — ready to go!")
